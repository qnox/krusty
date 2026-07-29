//! JPS (`.idea/` project model) provider. Pure static parse — no build tool runs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::jdk::{is_jdk_home, SystemEnvironment};
use super::model::{Module, ModuleId, ModuleOutput, ProjectModel, ProviderKind, SourceRoot};
use super::provider::{ProbeError, ProjectProvider};
use super::runner::CommandRunner;
use super::xml;
use crate::uri::file_uri_or_path;

const WATCH_GLOBS: &[&str] = &["**/.idea/misc.xml", "**/.idea/libraries/*.xml", "**/*.iml"];

fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn replace_path_macro(value: &mut String, name: &str, replacement: Option<&Path>) -> Option<()> {
    if value.contains(name) {
        *value = value.replace(name, &replacement?.to_string_lossy());
    }
    Some(())
}

/// Expand IntelliJ path macros. Returns `None` if a required value is
/// unavailable or a macro this reader cannot resolve remains.
fn expand_macro_text(raw: &str, project_dir: &Path, module_dir: &Path) -> Option<String> {
    let home = home_directory();
    let maven_repository = home.as_ref().map(|home| home.join(".m2/repository"));
    let mut replaced = raw.to_string();
    replace_path_macro(&mut replaced, "$PROJECT_DIR$", Some(project_dir))?;
    replace_path_macro(&mut replaced, "$MODULE_DIR$", Some(module_dir))?;
    replace_path_macro(
        &mut replaced,
        "$MAVEN_REPOSITORY$",
        maven_repository.as_deref(),
    )?;
    replace_path_macro(&mut replaced, "$USER_HOME$", home.as_deref())?;
    if replaced.contains('$') {
        return None;
    }
    Some(replaced)
}

fn expand_macros(raw: &str, project_dir: &Path, module_dir: &Path) -> Option<PathBuf> {
    expand_macro_text(raw, project_dir, module_dir).map(PathBuf::from)
}

fn url_to_path(url: &str, project_dir: &Path, module_dir: &Path) -> Option<PathBuf> {
    let normalized = if let Some(rest) = url.strip_prefix("jar://") {
        format!("file://{}", rest.strip_suffix("!/").unwrap_or(rest))
    } else {
        url.to_string()
    };
    let expanded = expand_macro_text(&normalized, project_dir, module_dir)?;
    file_uri_or_path(&expanded)
}

fn read_xml(path: &Path) -> Result<xml::Element, ProbeError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|error| ProbeError::Io(format!("{}: {error}", path.display())))?;
    parse_xml(path, &contents)
}

fn read_optional_xml(path: &Path) -> Result<Option<xml::Element>, ProbeError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ProbeError::Io(format!("{}: {error}", path.display()))),
    };
    parse_xml(path, &contents).map(Some)
}

fn parse_xml(path: &Path, contents: &str) -> Result<xml::Element, ProbeError> {
    xml::parse(contents)
        .ok_or_else(|| ProbeError::Parse(format!("{}: malformed XML", path.display())))
}

/// `library name -> resolved CLASSES jar paths` for every `.idea/libraries/*.xml`.
fn parse_library_table(root: &Path) -> Result<HashMap<String, Vec<PathBuf>>, ProbeError> {
    let mut table = HashMap::new();
    let directory = root.join(".idea").join("libraries");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(table),
        Err(error) => return Err(ProbeError::Io(format!("{}: {error}", directory.display()))),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("xml") {
            continue;
        }
        let document = read_xml(&path)?;
        for library in document.children_named("library") {
            let Some(name) = library.attr("name") else {
                continue;
            };
            let classpath = library_class_roots(library, root);
            table.entry(name.to_string()).or_insert(classpath);
        }
    }
    Ok(table)
}

/// Resolve every `CLASSES/root[@url]` under a `<library>` element. `module_dir`
/// is irrelevant for project-level libraries, so the project root is passed for
/// both — project libraries never reference `$MODULE_DIR$`.
fn library_class_roots(library: &xml::Element, project_dir: &Path) -> Vec<PathBuf> {
    let Some(classes) = library.child("CLASSES") else {
        return Vec::new();
    };
    classes
        .children_named("root")
        .filter_map(|root| root.attr("url"))
        .filter_map(|url| url_to_path(url, project_dir, project_dir))
        .collect()
}

fn language_level_to_target(level: &str) -> Option<String> {
    let version = level.strip_prefix("JDK_")?;
    let version = version.strip_suffix("_PREVIEW").unwrap_or(version);
    if version.chars().all(|character| character.is_ascii_digit()) {
        return Some(version.to_string());
    }
    if let Some(legacy) = version.strip_prefix("1_") {
        if legacy.chars().all(|character| character.is_ascii_digit()) {
            return Some(format!("1.{legacy}"));
        }
    }
    None
}

/// One resolved dependency edge or classpath entry.
struct OrderEntry {
    test_only: bool,
    exported: bool,
    module: Option<ModuleId>,
    classpath: Vec<PathBuf>,
}

/// Compile-scope entries exported to dependent modules.
#[derive(Default)]
struct ExportedDeps {
    modules: Vec<ModuleId>,
    classpath: Vec<PathBuf>,
}

#[cfg(test)]
fn parse_module(
    iml_path: &Path,
    project_dir: &Path,
    libraries: &HashMap<String, Vec<PathBuf>>,
    project_out: Option<&Path>,
    project_target: Option<&str>,
) -> Result<Vec<Module>, ProbeError> {
    parse_module_with_exports(
        iml_path,
        project_dir,
        libraries,
        project_out,
        project_target,
    )
    .map(|(modules, _)| modules)
}

fn parse_module_with_exports(
    iml_path: &Path,
    project_dir: &Path,
    libraries: &HashMap<String, Vec<PathBuf>>,
    project_out: Option<&Path>,
    project_target: Option<&str>,
) -> Result<(Vec<Module>, ExportedDeps), ProbeError> {
    // Listed modules may be absent from partial checkouts.
    let Some(document) = read_optional_xml(iml_path)? else {
        return Ok(Default::default());
    };
    let Some(name) = iml_path.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(Default::default());
    };
    let module_dir = iml_path.parent().unwrap_or(project_dir);
    let Some(manager) = document
        .children_named("component")
        .find(|component| component.attr("name") == Some("NewModuleRootManager"))
    else {
        return Ok(Default::default());
    };

    // Source roots, partitioned by isTestSource; resource folders skipped.
    let mut main_roots = Vec::new();
    let mut test_roots = Vec::new();
    for content in manager.children_named("content") {
        for folder in content.children_named("sourceFolder") {
            if folder
                .attr("type")
                .is_some_and(|kind| kind.contains("resource"))
            {
                continue;
            }
            let Some(url) = folder.attr("url") else {
                continue;
            };
            let Some(path) = url_to_path(url, project_dir, module_dir) else {
                continue;
            };
            let is_test = folder.attr("isTestSource") == Some("true");
            let mut root = if is_test {
                SourceRoot::test(path)
            } else {
                SourceRoot::source(path)
            };
            if folder.attr("generated") == Some("true") {
                root = root.generated();
            }
            if is_test {
                test_roots.push(root)
            } else {
                main_roots.push(root)
            }
        }
    }

    // Order entries, partitioned by scope.
    let mut entries = Vec::new();
    for order in manager.children_named("orderEntry") {
        // Runtime-only entries are not part of either compilation classpath.
        if order.attr("scope") == Some("RUNTIME") {
            continue;
        }
        let test_only = order.attr("scope") == Some("TEST");
        let exported = order.attr("exported").is_some();
        match order.attr("type") {
            Some("module") => {
                if let Some(target) = order.attr("module-name") {
                    entries.push(OrderEntry {
                        test_only,
                        exported,
                        module: Some(ModuleId::new(target, "main")),
                        classpath: Vec::new(),
                    });
                }
            }
            Some("library") => {
                if let Some(classpath) = order.attr("name").and_then(|name| libraries.get(name)) {
                    entries.push(OrderEntry {
                        test_only,
                        exported,
                        module: None,
                        classpath: classpath.clone(),
                    });
                }
            }
            Some("module-library") => {
                let classpath = order
                    .child("library")
                    .and_then(|library| library.child("CLASSES"))
                    .map(|classes| {
                        classes
                            .children_named("root")
                            .filter_map(|root| root.attr("url"))
                            .filter_map(|url| url_to_path(url, project_dir, module_dir))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                entries.push(OrderEntry {
                    test_only,
                    exported,
                    module: None,
                    classpath,
                });
            }
            _ => {}
        }
    }

    let jvm_target = manager
        .attr("LANGUAGE_LEVEL")
        .and_then(language_level_to_target)
        .or_else(|| project_target.map(str::to_string));

    let exports = ExportedDeps {
        modules: entries
            .iter()
            .filter(|e| e.exported && !e.test_only)
            .filter_map(|e| e.module.clone())
            .collect(),
        classpath: entries
            .iter()
            .filter(|e| e.exported && !e.test_only)
            .flat_map(|e| e.classpath.clone())
            .collect(),
    };

    let production_output = module_output(
        manager,
        "output",
        project_out,
        "production",
        name,
        project_dir,
        module_dir,
    );
    let test_output = module_output(
        manager,
        "output-test",
        project_out,
        "test",
        name,
        project_dir,
        module_dir,
    );

    // Main module: compile-scoped only.
    let mut main = Module::new(ModuleId::new(name, "main"), module_dir);
    main.display_name = name.to_string();
    main.source_roots = main_roots;
    main.classpath = dedup(
        entries
            .iter()
            .filter(|e| !e.test_only)
            .flat_map(|e| e.classpath.clone()),
    );
    main.depends_on = dedup(
        entries
            .iter()
            .filter(|e| !e.test_only)
            .filter_map(|e| e.module.clone()),
    );
    main.outputs = production_output
        .iter()
        .cloned()
        .map(ModuleOutput::Classes)
        .collect();
    main.jvm_target = jvm_target.clone();

    let has_test = !test_roots.is_empty() || entries.iter().any(|e| e.test_only);
    if !has_test {
        return Ok((vec![main], exports));
    }

    // Test module: compile + test-scoped.
    let mut test = Module::new(ModuleId::new(name, "test"), module_dir);
    test.display_name = format!("{name}:test");
    test.source_roots = test_roots;
    test.classpath = dedup(entries.iter().flat_map(|e| e.classpath.clone()));
    let mut test_deps = dedup(entries.iter().filter_map(|e| e.module.clone()));
    test_deps.push(ModuleId::new(name, "main"));
    test.depends_on = test_deps;
    test.outputs = test_output
        .iter()
        .cloned()
        .map(ModuleOutput::Classes)
        .collect();
    test.friend_paths = production_output.into_iter().collect();
    test.jvm_target = jvm_target;

    Ok((vec![main, test], exports))
}

fn expand_exported_deps(modules: &mut [Module], exported: &HashMap<ModuleId, ExportedDeps>) {
    for module in modules.iter_mut() {
        let mut queue: Vec<ModuleId> = module.depends_on.clone();
        let mut visited: std::collections::HashSet<ModuleId> =
            queue.iter().cloned().chain(module.id.clone()).collect();
        while let Some(dependency) = queue.pop() {
            let Some(exports) = exported.get(&dependency) else {
                continue;
            };
            for jar in &exports.classpath {
                if !module.classpath.contains(jar) {
                    module.classpath.push(jar.clone());
                }
            }
            for target in &exports.modules {
                if visited.insert(target.clone()) {
                    module.depends_on.push(target.clone());
                    queue.push(target.clone());
                }
            }
        }
    }
}

/// The explicit `<output>`/`<output-test>` url, or `<project_out>/<kind>/<name>`
/// when the module inherits the compiler output.
fn module_output(
    manager: &xml::Element,
    explicit: &str,
    project_out: Option<&Path>,
    kind: &str,
    name: &str,
    project_dir: &Path,
    module_dir: &Path,
) -> Option<PathBuf> {
    if let Some(url) = manager
        .child(explicit)
        .and_then(|output| output.attr("url"))
    {
        return url_to_path(url, project_dir, module_dir);
    }
    if manager.attr("inherit-compiler-output") == Some("true") {
        project_out.map(|out| out.join(kind).join(name))
    } else {
        None
    }
}

fn dedup<I, T>(items: I) -> Vec<T>
where
    I: IntoIterator<Item = T>,
    T: PartialEq,
{
    let mut result: Vec<T> = Vec::new();
    for item in items {
        if !result.contains(&item) {
            result.push(item);
        }
    }
    result
}

/// `(sdk name, resolved home)` for every `<jdk>` in a `jdk.table.xml`. Entries
/// whose `homePath` uses a macro this reader cannot resolve are dropped.
fn parse_jdk_table(content: &str) -> Vec<(String, PathBuf)> {
    let Some(document) = xml::parse(content) else {
        return Vec::new();
    };
    let mut table = Vec::new();
    for component in document.children_named("component") {
        if component.attr("name") != Some("ProjectJdkTable") {
            continue;
        }
        for jdk in component.children_named("jdk") {
            let name = jdk.child("name").and_then(|element| element.attr("value"));
            let home = jdk
                .child("homePath")
                .and_then(|element| element.attr("value"));
            if let (Some(name), Some(home)) = (name, home) {
                if let Some(path) = expand_home_macros(home) {
                    table.push((name.to_string(), path));
                }
            }
        }
    }
    table
}

/// Like `expand_macros`, but for `jdk.table.xml` home paths, which only use
/// `$USER_HOME$` (and the unresolvable `$APPLICATION_HOME_DIR$`).
fn expand_home_macros(raw: &str) -> Option<PathBuf> {
    let home = home_directory();
    let mut replaced = raw.to_string();
    replace_path_macro(&mut replaced, "$USER_HOME$", home.as_deref())?;
    if replaced.contains('$') {
        return None;
    }
    Some(PathBuf::from(replaced))
}

fn resolve_project_jdk(sdk_name: &str, tables: &[PathBuf]) -> Option<PathBuf> {
    let environment = SystemEnvironment;
    for table in tables {
        let Ok(contents) = std::fs::read_to_string(table) else {
            continue;
        };
        for (name, home) in parse_jdk_table(&contents) {
            if name == sdk_name && is_jdk_home(&environment, &home) {
                return Some(home);
            }
        }
    }
    None
}

/// Global `jdk.table.xml` candidates: every `JetBrains/*/options/jdk.table.xml`
/// under the per-OS config directory.
fn jdk_table_files() -> Vec<PathBuf> {
    let Some(home) = home_directory() else {
        return Vec::new();
    };
    let config_roots: Vec<PathBuf> = if cfg!(target_os = "macos") {
        vec![home
            .join("Library")
            .join("Application Support")
            .join("JetBrains")]
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(|appdata| vec![PathBuf::from(appdata).join("JetBrains")])
            .unwrap_or_default()
    } else {
        vec![home.join(".config").join("JetBrains")]
    };
    let mut files = Vec::new();
    for root in config_roots {
        let Ok(products) = std::fs::read_dir(&root) else {
            continue;
        };
        for product in products.flatten() {
            let candidate = product.path().join("options").join("jdk.table.xml");
            if candidate.is_file() {
                files.push(candidate);
            }
        }
    }
    files
}

#[derive(Default)]
struct ProjectSettings {
    output: Option<PathBuf>,
    jvm_target: Option<String>,
    sdk_name: Option<String>,
}

fn project_settings(root: &Path) -> Result<ProjectSettings, ProbeError> {
    let path = root.join(".idea").join("misc.xml");
    let Some(document) = read_optional_xml(&path)? else {
        return Ok(ProjectSettings::default());
    };
    let Some(manager) = document
        .children_named("component")
        .find(|component| component.attr("name") == Some("ProjectRootManager"))
    else {
        return Ok(ProjectSettings::default());
    };
    Ok(ProjectSettings {
        output: manager
            .child("output")
            .and_then(|output| output.attr("url"))
            .and_then(|url| url_to_path(url, root, root)),
        jvm_target: manager
            .attr("languageLevel")
            .and_then(language_level_to_target),
        sdk_name: manager.attr("project-jdk-name").map(str::to_string),
    })
}

/// The `.iml` paths listed in `.idea/modules.xml`.
fn iml_paths(root: &Path) -> Result<Vec<PathBuf>, ProbeError> {
    let path = root.join(".idea").join("modules.xml");
    let document = read_xml(&path)?;
    let mut paths = Vec::new();
    for component in document.children_named("component") {
        let Some(modules) = component.child("modules") else {
            continue;
        };
        for module in modules.children_named("module") {
            let resolved = module
                .attr("filepath")
                .and_then(|path| expand_macros(path, root, root))
                .or_else(|| {
                    module
                        .attr("fileurl")
                        .and_then(|url| url_to_path(url, root, root))
                });
            if let Some(resolved) = resolved {
                paths.push(resolved);
            }
        }
    }
    Ok(paths)
}

#[derive(Debug)]
pub struct JpsProvider {
    root: PathBuf,
}

impl JpsProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn probe_with_jdk_tables(&self, jdk_tables: &[PathBuf]) -> Result<ProjectModel, ProbeError> {
        let settings = project_settings(&self.root)?;
        let libraries = parse_library_table(&self.root)?;
        let mut modules = Vec::new();
        let mut exported = HashMap::new();
        for iml in iml_paths(&self.root)? {
            let (parsed, exports) = parse_module_with_exports(
                &iml,
                &self.root,
                &libraries,
                settings.output.as_deref(),
                settings.jvm_target.as_deref(),
            )?;
            if let Some(id) = parsed.first().and_then(|module| module.id.clone()) {
                exported.insert(id, exports);
            }
            modules.extend(parsed);
        }
        expand_exported_deps(&mut modules, &exported);
        let jdk_home = settings
            .sdk_name
            .as_deref()
            .and_then(|name| resolve_project_jdk(name, jdk_tables));
        Ok(ProjectModel {
            root: self.root.clone(),
            kind: ProviderKind::Jps,
            jdk_home,
            modules,
        })
    }
}

impl ProjectProvider for JpsProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Jps
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        let idea = self.root.join(".idea");
        let mut watched = vec![idea.join("modules.xml"), idea.join("misc.xml")];
        if let Ok(entries) = std::fs::read_dir(idea.join("libraries")) {
            watched.extend(entries.flatten().map(|entry| entry.path()));
        }
        for iml in iml_paths(&self.root).unwrap_or_default() {
            watched.push(iml);
        }
        watched
    }

    fn fingerprint_salt(&self) -> String {
        "jps-2".to_string()
    }

    fn additional_watch_globs(&self) -> &'static [&'static str] {
        WATCH_GLOBS
    }

    fn probe(&self, _runner: &dyn CommandRunner) -> Result<ProjectModel, ProbeError> {
        self.probe_with_jdk_tables(&jdk_table_files())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::model::{ModuleId, ModuleOutput, SourceRoot};

    fn app_iml() -> &'static str {
        r#"<module type="JAVA_MODULE" version="4">
             <component name="NewModuleRootManager" inherit-compiler-output="true" LANGUAGE_LEVEL="JDK_17">
               <content url="file://$MODULE_DIR$">
                 <sourceFolder url="file://$MODULE_DIR$/src/main/kotlin" isTestSource="false" />
                 <sourceFolder url="file://$MODULE_DIR$/build/generated" isTestSource="false" generated="true" />
                 <sourceFolder url="file://$MODULE_DIR$/src/test/kotlin" isTestSource="true" />
                 <sourceFolder url="file://$MODULE_DIR$/src/main/resources" type="java-resource" />
               </content>
               <content url="file://$MODULE_DIR$/shared">
                 <sourceFolder url="file://$MODULE_DIR$/shared/src" isTestSource="false" />
               </content>
               <orderEntry type="inheritedJdk" />
               <orderEntry type="sourceFolder" forTests="false" />
               <orderEntry type="module" module-name="core" />
               <orderEntry type="library" name="kotlin-stdlib" level="project" />
               <orderEntry type="library" name="junit" level="project" scope="TEST" />
               <orderEntry type="library" name="runtime" level="project" scope="RUNTIME" />
             </component>
           </module>"#
    }

    #[test]
    fn an_iml_splits_into_main_and_test_modules_partitioned_by_scope() {
        let tree = crate::project::testing::TempTree::new("jps-iml");
        let iml = tree.write("app/app.iml", app_iml());
        let mut libraries = HashMap::new();
        libraries.insert(
            "kotlin-stdlib".to_string(),
            vec![PathBuf::from("/m2/kotlin-stdlib.jar")],
        );
        libraries.insert("junit".to_string(), vec![PathBuf::from("/m2/junit.jar")]);
        libraries.insert(
            "runtime".to_string(),
            vec![PathBuf::from("/m2/runtime.jar")],
        );
        let project_out = tree.path("out");

        let modules = parse_module(
            &iml,
            tree.root(),
            &libraries,
            Some(&project_out),
            Some("11"),
        )
        .unwrap();

        let main = modules
            .iter()
            .find(|module| module.id == Some(ModuleId::new("app", "main")))
            .unwrap();
        assert_eq!(
            main.source_roots,
            vec![
                SourceRoot::source(tree.path("app/src/main/kotlin")),
                SourceRoot::source(tree.path("app/build/generated")).generated(),
                SourceRoot::source(tree.path("app/shared/src")),
            ]
        );
        // Compile-scoped only; the TEST library is absent from main.
        assert_eq!(main.classpath, vec![PathBuf::from("/m2/kotlin-stdlib.jar")]);
        assert_eq!(main.depends_on, vec![ModuleId::new("core", "main")]);
        assert_eq!(
            main.outputs,
            vec![ModuleOutput::classes(tree.path("out/production/app"))]
        );
        // LANGUAGE_LEVEL on the module wins over the project target.
        assert_eq!(main.jvm_target.as_deref(), Some("17"));

        let test = modules
            .iter()
            .find(|module| module.id == Some(ModuleId::new("app", "test")))
            .unwrap();
        assert_eq!(
            test.source_roots,
            vec![SourceRoot::test(tree.path("app/src/test/kotlin"))]
        );
        // Test sees compile + test-scoped libraries.
        assert_eq!(
            test.classpath,
            vec![
                PathBuf::from("/m2/kotlin-stdlib.jar"),
                PathBuf::from("/m2/junit.jar")
            ]
        );
        assert!(test.depends_on.contains(&ModuleId::new("app", "main")));
        assert!(test.depends_on.contains(&ModuleId::new("core", "main")));
        assert_eq!(test.friend_paths, vec![tree.path("out/production/app")]);
        assert_eq!(
            test.outputs,
            vec![ModuleOutput::classes(tree.path("out/test/app"))]
        );
    }

    #[test]
    fn a_module_with_no_test_sources_or_test_deps_produces_only_a_main_module() {
        let tree = crate::project::testing::TempTree::new("jps-iml-notest");
        let iml = tree.write(
            "core/core.iml",
            r#"<module>
                 <component name="NewModuleRootManager" inherit-compiler-output="true">
                   <content url="file://$MODULE_DIR$">
                     <sourceFolder url="file://$MODULE_DIR$/src/main/kotlin" isTestSource="false" />
                   </content>
                   <orderEntry type="library" name="kotlin-stdlib" level="project" />
                 </component>
               </module>"#,
        );
        let mut libraries = HashMap::new();
        libraries.insert(
            "kotlin-stdlib".to_string(),
            vec![PathBuf::from("/m2/kotlin-stdlib.jar")],
        );
        let project_out = tree.path("out");

        let modules =
            parse_module(&iml, tree.root(), &libraries, Some(&project_out), None).unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].id, Some(ModuleId::new("core", "main")));
    }

    #[test]
    fn a_missing_listed_iml_yields_no_modules() {
        let tree = crate::project::testing::TempTree::new("jps-missing-iml");
        let modules = parse_module(
            &tree.path("missing.iml"),
            tree.root(),
            &HashMap::new(),
            None,
            None,
        )
        .unwrap();
        assert!(modules.is_empty());
    }

    #[test]
    fn a_malformed_listed_iml_fails_the_probe_input() {
        let tree = crate::project::testing::TempTree::new("jps-bad-iml");
        let iml = tree.write("core/core.iml", "<module><component></module>");
        let error = parse_module(&iml, tree.root(), &HashMap::new(), None, None).unwrap_err();
        assert!(matches!(error, ProbeError::Parse(_)));
    }

    #[test]
    fn language_levels_map_to_jvm_targets() {
        assert_eq!(language_level_to_target("JDK_1_8").as_deref(), Some("1.8"));
        assert_eq!(language_level_to_target("JDK_17").as_deref(), Some("17"));
        assert_eq!(
            language_level_to_target("JDK_21_PREVIEW").as_deref(),
            Some("21")
        );
        assert_eq!(language_level_to_target("JDK_11").as_deref(), Some("11"));
        assert_eq!(language_level_to_target("JDK_17_BROKEN"), None);
        assert_eq!(language_level_to_target("garbage"), None);
    }

    #[test]
    fn file_and_jar_urls_expand_project_and_module_macros() {
        let project = Path::new("/p");
        let module = Path::new("/p/app");
        assert_eq!(
            url_to_path("file://$MODULE_DIR$/src/main/kotlin", project, module),
            Some(PathBuf::from("/p/app/src/main/kotlin"))
        );
        assert_eq!(
            url_to_path("jar://$PROJECT_DIR$/libs/foo.jar!/", project, module),
            Some(PathBuf::from("/p/libs/foo.jar"))
        );
        assert_eq!(
            url_to_path(
                "file://$MODULE_DIR$/src/project%20with%20%23hash",
                project,
                module
            ),
            Some(PathBuf::from("/p/app/src/project with #hash"))
        );
    }

    #[test]
    fn an_unresolvable_macro_yields_none() {
        let project = Path::new("/p");
        let module = Path::new("/p/app");
        assert_eq!(
            url_to_path(
                "jar://$APPLICATION_HOME_DIR$/lib/kotlin-stdlib.jar!/",
                project,
                module
            ),
            None
        );
    }

    #[test]
    fn an_unavailable_macro_value_yields_none_instead_of_a_relative_path() {
        let mut value = "$USER_HOME$/lib/a.jar".to_string();
        assert_eq!(replace_path_macro(&mut value, "$USER_HOME$", None), None);
    }

    fn multi_module_tree() -> crate::project::testing::TempTree {
        let tree = crate::project::testing::TempTree::new("jps-probe");
        tree.write(
            ".idea/modules.xml",
            r#"<project version="4">
                 <component name="ProjectModuleManager">
                   <modules>
                     <module fileurl="file://$PROJECT_DIR$/core/core.iml" filepath="$PROJECT_DIR$/core/core.iml" />
                     <module fileurl="file://$PROJECT_DIR$/app/app.iml" filepath="$PROJECT_DIR$/app/app.iml" />
                   </modules>
                 </component>
               </project>"#,
        );
        tree.write(
            ".idea/misc.xml",
            r#"<project version="4">
                 <component name="ProjectRootManager" languageLevel="JDK_17" project-jdk-name="temurin-17">
                   <output url="file://$PROJECT_DIR$/out" />
                 </component>
               </project>"#,
        );
        tree.write(
            ".idea/libraries/kotlin_stdlib.xml",
            r#"<component name="libraryTable">
                 <library name="kotlin-stdlib">
                   <CLASSES><root url="jar://$PROJECT_DIR$/libs/kotlin-stdlib.jar!/" /></CLASSES>
                 </library>
               </component>"#,
        );
        tree.write(
            "core/core.iml",
            r#"<module>
                 <component name="NewModuleRootManager" inherit-compiler-output="true">
                   <content url="file://$MODULE_DIR$">
                     <sourceFolder url="file://$MODULE_DIR$/src/main/kotlin" isTestSource="false" />
                   </content>
                   <orderEntry type="library" name="kotlin-stdlib" level="project" />
                 </component>
               </module>"#,
        );
        tree.write(
            "app/app.iml",
            r#"<module>
                 <component name="NewModuleRootManager">
                   <output url="file://$MODULE_DIR$/out/production/app" />
                   <content url="file://$MODULE_DIR$">
                     <sourceFolder url="file://$MODULE_DIR$/src/main/kotlin" isTestSource="false" />
                   </content>
                   <orderEntry type="module" module-name="core" />
                 </component>
               </module>"#,
        );
        tree
    }

    #[test]
    fn the_probe_reads_the_full_idea_model() {
        let tree = multi_module_tree();
        let model = JpsProvider::new(tree.root())
            .probe_with_jdk_tables(&[])
            .unwrap();

        assert_eq!(model.kind, ProviderKind::Jps);

        let core = model.module(&ModuleId::new("core", "main")).unwrap();
        assert_eq!(core.classpath, vec![tree.path("libs/kotlin-stdlib.jar")]);
        assert_eq!(core.jvm_target.as_deref(), Some("17")); // inherited from the project.
        assert_eq!(
            core.outputs,
            vec![ModuleOutput::classes(tree.path("out/production/core"))]
        );

        let app = model.module(&ModuleId::new("app", "main")).unwrap();
        assert_eq!(app.depends_on, vec![ModuleId::new("core", "main")]);
        // Explicit <output> honored.
        assert_eq!(
            app.outputs,
            vec![ModuleOutput::classes(tree.path("app/out/production/app"))]
        );
    }

    #[test]
    fn exported_module_deps_propagate_transitively_to_dependents() {
        let tree = crate::project::testing::TempTree::new("jps-exported");
        tree.write(
            ".idea/modules.xml",
            r#"<project version="4">
                 <component name="ProjectModuleManager">
                   <modules>
                     <module fileurl="file://$PROJECT_DIR$/impl/impl.iml" filepath="$PROJECT_DIR$/impl/impl.iml" />
                     <module fileurl="file://$PROJECT_DIR$/java/java.iml" filepath="$PROJECT_DIR$/java/java.iml" />
                     <module fileurl="file://$PROJECT_DIR$/ide/ide.iml" filepath="$PROJECT_DIR$/ide/ide.iml" />
                     <module fileurl="file://$PROJECT_DIR$/editor/editor.iml" filepath="$PROJECT_DIR$/editor/editor.iml" />
                     <module fileurl="file://$PROJECT_DIR$/xmldom/xmldom.iml" filepath="$PROJECT_DIR$/xmldom/xmldom.iml" />
                   </modules>
                 </component>
               </project>"#,
        );
        tree.write(
            ".idea/libraries/streamex.xml",
            r#"<component name="libraryTable">
                 <library name="StreamEx">
                   <CLASSES><root url="jar://$PROJECT_DIR$/libs/streamex.jar!/" /></CLASSES>
                 </library>
               </component>"#,
        );
        let leaf_iml = r#"<module>
                 <component name="NewModuleRootManager" inherit-compiler-output="true">
                   <content url="file://$MODULE_DIR$">
                     <sourceFolder url="file://$MODULE_DIR$/src" isTestSource="false" />
                   </content>
                 </component>
               </module>"#;
        tree.write("editor/editor.iml", leaf_iml);
        tree.write("xmldom/xmldom.iml", leaf_iml);
        tree.write(
            "ide/ide.iml",
            r#"<module>
                 <component name="NewModuleRootManager" inherit-compiler-output="true">
                   <content url="file://$MODULE_DIR$">
                     <sourceFolder url="file://$MODULE_DIR$/src" isTestSource="false" />
                   </content>
                   <orderEntry type="module" module-name="editor" exported="" />
                 </component>
               </module>"#,
        );
        tree.write(
            "java/java.iml",
            r#"<module>
                 <component name="NewModuleRootManager" inherit-compiler-output="true">
                   <content url="file://$MODULE_DIR$">
                     <sourceFolder url="file://$MODULE_DIR$/src" isTestSource="false" />
                   </content>
                   <orderEntry type="module" module-name="ide" exported="" />
                   <orderEntry type="module" module-name="xmldom" />
                   <orderEntry type="library" name="StreamEx" level="project" exported="" />
                 </component>
               </module>"#,
        );
        tree.write(
            "impl/impl.iml",
            r#"<module>
                 <component name="NewModuleRootManager" inherit-compiler-output="true">
                   <content url="file://$MODULE_DIR$">
                     <sourceFolder url="file://$MODULE_DIR$/src" isTestSource="false" />
                   </content>
                   <orderEntry type="module" module-name="java" />
                 </component>
               </module>"#,
        );

        let model = JpsProvider::new(tree.root())
            .probe_with_jdk_tables(&[])
            .unwrap();

        let implementation = model.module(&ModuleId::new("impl", "main")).unwrap();
        assert!(implementation
            .depends_on
            .contains(&ModuleId::new("java", "main")));
        assert!(implementation
            .depends_on
            .contains(&ModuleId::new("ide", "main")));
        assert!(implementation
            .depends_on
            .contains(&ModuleId::new("editor", "main")));
        assert!(!implementation
            .depends_on
            .contains(&ModuleId::new("xmldom", "main")));
        assert!(implementation
            .classpath
            .contains(&tree.path("libs/streamex.jar")));
        let java = model.module(&ModuleId::new("java", "main")).unwrap();
        assert!(java.depends_on.contains(&ModuleId::new("xmldom", "main")));
    }

    #[test]
    fn the_probe_resolves_an_injected_jdk_table() {
        let tree = multi_module_tree();
        tree.write("fake-jdk/lib/modules", "");
        let table = tree.write(
            "jdk.table.xml",
            &format!(
                r#"<application><component name="ProjectJdkTable">
                     <jdk><name value="temurin-17" /><homePath value="{}" /></jdk>
                   </component></application>"#,
                tree.path("fake-jdk").display()
            ),
        );

        let model = JpsProvider::new(tree.root())
            .probe_with_jdk_tables(std::slice::from_ref(&table))
            .unwrap();

        assert_eq!(model.jdk_home, Some(tree.path("fake-jdk")));
    }

    #[test]
    fn the_probe_skips_listed_imls_that_are_absent_from_the_checkout() {
        let tree = multi_module_tree();
        tree.write(
            ".idea/modules.xml",
            r#"<project version="4">
                 <component name="ProjectModuleManager">
                   <modules>
                     <module fileurl="file://$PROJECT_DIR$/core/core.iml" filepath="$PROJECT_DIR$/core/core.iml" />
                     <module fileurl="file://$PROJECT_DIR$/app/app.iml" filepath="$PROJECT_DIR$/app/app.iml" />
                     <module fileurl="file://$PROJECT_DIR$/prebuilts/gen.iml" filepath="$PROJECT_DIR$/prebuilts/gen.iml" />
                   </modules>
                 </component>
               </project>"#,
        );

        let model = JpsProvider::new(tree.root())
            .probe_with_jdk_tables(&[])
            .unwrap();

        assert!(model.module(&ModuleId::new("core", "main")).is_some());
        assert!(model.module(&ModuleId::new("app", "main")).is_some());
        assert!(model.module(&ModuleId::new("gen", "main")).is_none());
    }

    #[test]
    fn a_malformed_modules_file_is_a_parse_error() {
        use crate::project::runner::testing::FakeRunner;

        let tree = crate::project::testing::TempTree::new("jps-bad-modules");
        tree.write(".idea/modules.xml", "<project><modules></project>"); // unclosed
        let error = JpsProvider::new(tree.root())
            .probe(&FakeRunner::default())
            .unwrap_err();
        assert!(matches!(error, ProbeError::Parse(_)));
    }

    #[test]
    fn watch_paths_list_the_idea_model_files() {
        let tree = multi_module_tree();
        let provider = JpsProvider::new(tree.root());
        let watched = provider.watch_paths();
        assert!(watched.contains(&tree.path(".idea/modules.xml")));
        assert!(watched.contains(&tree.path(".idea/misc.xml")));
        assert!(watched.contains(&tree.path("app/app.iml")));
        assert!(provider.watch_globs().contains(&"**/*.iml".to_string()));
    }

    #[test]
    fn a_jdk_table_maps_the_sdk_name_to_a_home_path() {
        let home = home_directory().unwrap_or_default();
        let content = r#"<application>
            <component name="ProjectJdkTable">
              <jdk version="2">
                <name value="temurin-17" />
                <type value="JavaSDK" />
                <homePath value="$USER_HOME$/.sdkman/candidates/java/17-tem" />
              </jdk>
              <jdk version="2">
                <name value="bundled" />
                <homePath value="$APPLICATION_HOME_DIR$/jbr" />
              </jdk>
            </component>
          </application>"#;
        let table = parse_jdk_table(content);
        assert!(table.contains(&(
            "temurin-17".to_string(),
            home.join(".sdkman/candidates/java/17-tem"),
        )));
        // The unresolvable $APPLICATION_HOME_DIR$ entry is dropped.
        assert!(table.iter().all(|(name, _)| name != "bundled"));
    }

    #[test]
    fn resolve_project_jdk_validates_the_home_and_falls_back_to_none() {
        let tree = crate::project::testing::TempTree::new("jps-jdk");
        // A real (fake) JDK home: lib/modules exists.
        tree.write("jdk/lib/modules", "");
        let table = tree.write(
            "jdk.table.xml",
            &format!(
                r#"<application><component name="ProjectJdkTable">
                     <jdk><name value="proj-jdk" /><homePath value="{}" /></jdk>
                   </component></application>"#,
                tree.path("jdk").display()
            ),
        );

        assert_eq!(
            resolve_project_jdk("proj-jdk", std::slice::from_ref(&table)),
            Some(tree.path("jdk"))
        );
        // Unknown name, or a home without lib/modules, resolves to None.
        assert_eq!(resolve_project_jdk("absent", &[table]), None);
    }

    #[test]
    fn libraries_are_keyed_on_the_inner_name_not_the_filename() {
        let tree = crate::project::testing::TempTree::new("jps-libs");
        // Filename mangled; inner name is authoritative.
        tree.write(
            ".idea/libraries/Kotlin_stdlib.xml",
            r#"<component name="libraryTable">
                 <library name="kotlin-stdlib">
                   <CLASSES>
                     <root url="jar://$MAVEN_REPOSITORY$/org/jetbrains/kotlin/kotlin-stdlib/1.9.0/kotlin-stdlib-1.9.0.jar!/" />
                   </CLASSES>
                 </library>
               </component>"#,
        );
        let home = home_directory().unwrap_or_default();
        let expected = home.join(
            ".m2/repository/org/jetbrains/kotlin/kotlin-stdlib/1.9.0/kotlin-stdlib-1.9.0.jar",
        );

        let table = parse_library_table(tree.root()).unwrap();
        assert_eq!(table.get("kotlin-stdlib"), Some(&vec![expected]));
        assert_eq!(table.get("Kotlin_stdlib"), None);
    }
}
