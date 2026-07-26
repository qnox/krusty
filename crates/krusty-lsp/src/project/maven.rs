//! Maven project-model provider using effective POM and dependency plugin output.

use std::path::{Path, PathBuf};

use super::fingerprint::collect_build_files;
use super::model::{Module, ModuleId, ModuleOutput, ProjectModel, ProviderKind, SourceRoot};
use super::provider::{ProbeError, ProjectProvider};
use super::runner::{Command, CommandRunner, MAVEN};
use super::xml::{self, Element};

const PROBE_VERSION: &str = "3";
const COMPILE_CLASSPATH_FILE: &str = ".krusty-classpath.txt";
const TEST_CLASSPATH_FILE: &str = ".krusty-classpath-test.txt";

#[derive(Debug)]
pub struct MavenProvider {
    root: PathBuf,
    mvn: PathBuf,
    effective_pom: PathBuf,
}

/// One reactor module: its directory, coordinates, and parsed POM.
#[derive(Debug)]
struct ReactorModule {
    directory: PathBuf,
    group_id: String,
    artifact_id: String,
    packaging: String,
    pom: Element,
}

impl ReactorModule {
    fn coordinates(&self) -> String {
        format!("{}:{}", self.group_id, self.artifact_id)
    }

    fn is_aggregator(&self) -> bool {
        self.packaging == "pom"
    }

    fn property(&self, name: &str) -> Option<&str> {
        self.pom.text_at(&["properties", name])
    }

    fn build_value(&self, key: &str) -> Option<&str> {
        self.pom.text_at(&["build", key])
    }

    fn directory_of(&self, configured: Option<&str>, default: &str) -> PathBuf {
        let relative = configured.unwrap_or(default);
        let path = Path::new(relative);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.directory.join(path)
        }
    }
}

impl MavenProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let mvn = MAVEN.resolve(&root);
        let effective_pom = effective_pom_path(&root);
        Self {
            root,
            mvn,
            effective_pom,
        }
    }

    pub fn classpath_command(&self, scope: &str, output_file: &str) -> Command {
        Command::new(self.mvn.clone(), self.root.clone()).args([
            "-B".to_string(),
            "--quiet".to_string(),
            "dependency:build-classpath".to_string(),
            format!("-Dmdep.outputFile={output_file}"),
            format!("-Dmdep.includeScope={scope}"),
        ])
    }

    fn reactor(&self) -> Result<Vec<ReactorModule>, ProbeError> {
        let contents = std::fs::read_to_string(&self.effective_pom).map_err(|error| {
            ProbeError::Io(format!("{}: {error}", self.effective_pom.display()))
        })?;
        parse_effective_reactor(&self.root, &contents)
    }

    fn run(&self, runner: &dyn CommandRunner, command: &Command) -> Result<(), ProbeError> {
        let output = runner
            .run(command)
            .map_err(|error| ProbeError::Io(format!("{}: {error}", self.mvn.display())))?;
        if output.succeeded() {
            return Ok(());
        }
        Err(ProbeError::Tool {
            program: self.mvn.to_string_lossy().into_owned(),
            status: output.status,
            message: first_error_line(&output.stdout, &output.stderr),
        })
    }

    fn effective_pom_command(&self) -> Command {
        Command::new(self.mvn.clone(), self.root.clone()).args([
            "-B".to_string(),
            "--quiet".to_string(),
            "help:effective-pom".to_string(),
            format!("-Doutput={}", self.effective_pom.to_string_lossy()),
        ])
    }

    fn cleanup_probe_files(&self) {
        std::fs::remove_file(&self.effective_pom).ok();
        for pom in pom_files(&self.root) {
            let Some(directory) = pom.parent() else {
                continue;
            };
            std::fs::remove_file(directory.join(COMPILE_CLASSPATH_FILE)).ok();
            std::fs::remove_file(directory.join(TEST_CLASSPATH_FILE)).ok();
        }
    }
}

impl ProjectProvider for MavenProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Maven
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        let mut watched = collect_build_files(&self.root, 8, &|path| {
            path.file_name().is_some_and(|name| name == "pom.xml")
                || path
                    .parent()
                    .is_some_and(|parent| parent.file_name().is_some_and(|name| name == ".mvn"))
        });
        if let Some(home) = home_directory() {
            watched.push(home.join(".m2/settings.xml"));
            watched.push(home.join(".m2/toolchains.xml"));
        }
        watched
    }

    fn fingerprint_salt(&self) -> String {
        format!("maven-{PROBE_VERSION}")
    }

    fn probe(&self, runner: &dyn CommandRunner) -> Result<ProjectModel, ProbeError> {
        let result = (|| {
            self.run(
                runner,
                &self.classpath_command("compile", COMPILE_CLASSPATH_FILE),
            )?;
            self.run(runner, &self.classpath_command("test", TEST_CLASSPATH_FILE))?;
            self.run(runner, &self.effective_pom_command())?;

            let reactor = self.reactor()?;
            let mut modules = Vec::new();
            for entry in reactor.iter().filter(|entry| !entry.is_aggregator()) {
                modules.push(main_module(entry, &reactor));
                let tests = test_module(entry, &reactor);
                if !tests.source_roots.is_empty() {
                    modules.push(tests);
                }
            }
            Ok(ProjectModel {
                root: self.root.clone(),
                kind: ProviderKind::Maven,
                jdk_home: None,
                modules,
            })
        })();
        self.cleanup_probe_files();
        result
    }
}

fn parse_effective_reactor(root: &Path, input: &str) -> Result<Vec<ReactorModule>, ProbeError> {
    let document = xml::parse(input)
        .ok_or_else(|| ProbeError::Parse("malformed effective POM XML".to_string()))?;
    let projects: Vec<Element> = if document.name == "projects" {
        document.children_named("project").cloned().collect()
    } else if document.name == "project" {
        vec![document]
    } else {
        return Err(ProbeError::Parse(format!(
            "expected project or projects in effective POM, found {}",
            document.name
        )));
    };
    let source_poms = source_pom_coordinates(root);
    let mut modules = Vec::new();
    for pom in projects {
        let group_id = pom
            .text_at(&["groupId"])
            .or_else(|| pom.text_at(&["parent", "groupId"]))
            .unwrap_or_default()
            .to_string();
        let artifact_id = pom
            .text_at(&["artifactId"])
            .ok_or_else(|| ProbeError::Parse("effective POM has no artifactId".to_string()))?
            .to_string();
        let matching: Vec<&(PathBuf, String, String)> = source_poms
            .iter()
            .filter(|(_, group, artifact)| group == &group_id && artifact == &artifact_id)
            .collect();
        let directory = match matching.as_slice() {
            [(path, _, _)] => path.clone(),
            _ => {
                let by_artifact: Vec<&PathBuf> = source_poms
                    .iter()
                    .filter(|(_, _, artifact)| artifact == &artifact_id)
                    .map(|(path, _, _)| path)
                    .collect();
                match by_artifact.as_slice() {
                    [path] => (*path).clone(),
                    _ => {
                        return Err(ProbeError::Parse(format!(
                            "cannot map effective project {group_id}:{artifact_id} to a pom.xml"
                        )));
                    }
                }
            }
        };
        modules.push(ReactorModule {
            directory,
            group_id,
            artifact_id,
            packaging: pom.text_at(&["packaging"]).unwrap_or("jar").to_string(),
            pom,
        });
    }
    if modules.is_empty() {
        return Err(ProbeError::Parse(
            "effective POM contains no projects".to_string(),
        ));
    }
    Ok(modules)
}

fn pom_files(root: &Path) -> Vec<PathBuf> {
    collect_build_files(root, 8, &|path| {
        path.file_name().is_some_and(|name| name == "pom.xml")
    })
}

fn source_pom_coordinates(root: &Path) -> Vec<(PathBuf, String, String)> {
    pom_files(root)
        .into_iter()
        .filter_map(|path| {
            let pom = std::fs::read_to_string(&path)
                .ok()
                .and_then(|contents| xml::parse(&contents))?;
            let directory = path.parent()?.to_path_buf();
            let group = pom
                .text_at(&["groupId"])
                .or_else(|| pom.text_at(&["parent", "groupId"]))
                .unwrap_or_default()
                .to_string();
            let artifact = pom.text_at(&["artifactId"])?.to_string();
            Some((directory, group, artifact))
        })
        .collect()
}

fn effective_pom_path(root: &Path) -> PathBuf {
    let mut hasher = super::fingerprint::Hasher::default();
    hasher.write_str(&root.to_string_lossy());
    std::env::temp_dir().join(format!(
        "krusty-maven-{:016x}-{}.xml",
        hasher.finish().as_u64(),
        std::process::id()
    ))
}

fn main_module(entry: &ReactorModule, reactor: &[ReactorModule]) -> Module {
    let mut module = Module::new(
        ModuleId::new(&entry.coordinates(), "main"),
        &entry.directory,
    );
    module.display_name = entry.artifact_id.clone();
    module.source_roots = source_roots(
        entry,
        entry.build_value("sourceDirectory"),
        &["src/main/java", "src/main/kotlin"],
        "target/generated-sources",
        false,
    );
    module.outputs = vec![ModuleOutput::classes(
        entry.directory_of(entry.build_value("outputDirectory"), "target/classes"),
    )];
    module.classpath = classpath_of(entry, COMPILE_CLASSPATH_FILE, reactor);
    module.jvm_target = jvm_target(entry);
    module.depends_on = reactor_dependencies(entry, reactor);
    module
}

fn test_module(entry: &ReactorModule, reactor: &[ReactorModule]) -> Module {
    let mut module = Module::new(
        ModuleId::new(&entry.coordinates(), "test"),
        &entry.directory,
    );
    module.display_name = format!("{}:test", entry.artifact_id);
    module.source_roots = source_roots(
        entry,
        entry.build_value("testSourceDirectory"),
        &["src/test/java", "src/test/kotlin"],
        "target/generated-test-sources",
        true,
    );
    module.outputs = vec![ModuleOutput::classes(entry.directory_of(
        entry.build_value("testOutputDirectory"),
        "target/test-classes",
    ))];
    module.classpath = classpath_of(entry, TEST_CLASSPATH_FILE, reactor);
    module.jvm_target = jvm_target(entry);
    module.depends_on = reactor_dependencies(entry, reactor);
    let own_main = ModuleId::new(&entry.coordinates(), "main");
    module.depends_on.push(own_main);
    // Test code sees `internal` declarations of the module it tests.
    module.friend_paths =
        vec![entry.directory_of(entry.build_value("outputDirectory"), "target/classes")];
    module
}

/// Configured or conventional roots that exist, plus whatever a code generator wrote under the
/// generated-sources directory — that scan is what makes `addCompileSourceRoot` visible without
/// reimplementing Maven's plugin API.
fn source_roots(
    entry: &ReactorModule,
    configured: Option<&str>,
    defaults: &[&str],
    generated_root: &str,
    tests: bool,
) -> Vec<SourceRoot> {
    let mut roots = Vec::new();
    let push = |path: PathBuf, generated: bool, roots: &mut Vec<SourceRoot>| {
        if !path.is_dir() || roots.iter().any(|root: &SourceRoot| root.path == path) {
            return;
        }
        let root = if tests {
            SourceRoot::test(path)
        } else {
            SourceRoot::source(path)
        };
        roots.push(if generated { root.generated() } else { root });
    };

    match configured {
        Some(configured) => push(entry.directory_of(Some(configured), ""), false, &mut roots),
        None => {
            for default in defaults {
                push(entry.directory.join(default), false, &mut roots);
            }
        }
    }
    for source_dir in kotlin_source_dirs(entry) {
        push(source_dir, false, &mut roots);
    }
    if let Ok(entries) = std::fs::read_dir(entry.directory.join(generated_root)) {
        let mut generated: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        generated.sort();
        for path in generated {
            push(path, true, &mut roots);
        }
    }
    roots
}

/// `<sourceDirs>` configured on `kotlin-maven-plugin`, the usual way a Maven build points Kotlin at
/// `src/main/kotlin` when the Java source directory is configured explicitly.
fn kotlin_source_dirs(entry: &ReactorModule) -> Vec<PathBuf> {
    let Some(plugins) = entry.pom.element_at(&["build", "plugins"]) else {
        return Vec::new();
    };
    plugins
        .children_named("plugin")
        .filter(|plugin| plugin.text_at(&["artifactId"]) == Some("kotlin-maven-plugin"))
        .filter_map(|plugin| plugin.element_at(&["configuration", "sourceDirs"]))
        .flat_map(|dirs| dirs.children_named("sourceDir"))
        .map(|dir| entry.directory_of(Some(dir.text.trim()), ""))
        .collect()
}

fn jvm_target(entry: &ReactorModule) -> Option<String> {
    entry
        .property("kotlin.compiler.jvmTarget")
        .or_else(|| entry.property("maven.compiler.release"))
        .or_else(|| entry.property("maven.compiler.target"))
        .map(str::to_string)
}

/// Read the file Maven wrote, mapping any reactor artifact jar onto the sibling module's output
/// directory: in an editor session the sibling's classes are current, and its jar may not exist at
/// all when the module was never installed.
fn classpath_of(entry: &ReactorModule, file: &str, reactor: &[ReactorModule]) -> Vec<PathBuf> {
    let Ok(contents) = std::fs::read_to_string(entry.directory.join(file)) else {
        return Vec::new();
    };
    let mut classpath = Vec::new();
    for path in std::env::split_paths(contents.trim()) {
        if path.as_os_str().is_empty() {
            continue;
        }
        let entry = reactor_output_for(&path, reactor).unwrap_or(path);
        if !classpath.contains(&entry) {
            classpath.push(entry);
        }
    }
    classpath
}

fn reactor_output_for(path: &Path, reactor: &[ReactorModule]) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    reactor
        .iter()
        .filter(|module| !module.is_aggregator() && reactor_artifact_matches(path, name, module))
        .max_by_key(|module| module.artifact_id.len())
        .map(|module| {
            module.directory_of(
                module.pom.text_at(&["build", "outputDirectory"]),
                "target/classes",
            )
        })
}

fn reactor_artifact_matches(path: &Path, file_name: &str, module: &ReactorModule) -> bool {
    if !file_name.starts_with(&format!("{}-", module.artifact_id)) {
        return false;
    }
    let Some(artifact_directory) = path.parent().and_then(Path::parent) else {
        return false;
    };
    if artifact_directory
        .file_name()
        .and_then(|name| name.to_str())
        != Some(module.artifact_id.as_str())
    {
        return false;
    }
    let mut ancestors = artifact_directory.parent();
    for segment in module.group_id.rsplit('.') {
        let Some(directory) = ancestors else {
            return false;
        };
        if directory.file_name().and_then(|name| name.to_str()) != Some(segment) {
            return false;
        }
        ancestors = directory.parent();
    }
    true
}

fn reactor_dependencies(entry: &ReactorModule, reactor: &[ReactorModule]) -> Vec<ModuleId> {
    let Some(dependencies) = entry.pom.element_at(&["dependencies"]) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for dependency in dependencies.children_named("dependency") {
        let (Some(group), Some(artifact)) = (
            dependency.text_at(&["groupId"]),
            dependency.text_at(&["artifactId"]),
        ) else {
            continue;
        };
        let coordinates = format!("{group}:{artifact}");
        if reactor
            .iter()
            .any(|module| !module.is_aggregator() && module.coordinates() == coordinates)
        {
            let id = ModuleId::new(&coordinates, "main");
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn first_error_line(stdout: &str, stderr: &str) -> String {
    stdout
        .lines()
        .chain(stderr.lines())
        .map(str::trim)
        .find(|line| line.starts_with("[ERROR]"))
        .or_else(|| {
            stderr
                .lines()
                .chain(stdout.lines())
                .map(str::trim)
                .find(|line| !line.is_empty())
        })
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::runner::testing::FakeRunner;
    use crate::project::testing::TempTree;

    fn classpath_line(entries: &[&str]) -> String {
        std::env::join_paths(entries)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn reactor_fixture() -> TempTree {
        let tree = TempTree::new("maven-reactor");
        tree.write(
            "pom.xml",
            r#"<project>
                 <groupId>com.example</groupId>
                 <artifactId>parent</artifactId>
                 <packaging>pom</packaging>
                 <properties><maven.compiler.release>21</maven.compiler.release></properties>
                 <modules><module>core</module><module>app</module></modules>
               </project>"#,
        );
        tree.write(
            "core/pom.xml",
            r#"<project>
                 <parent><groupId>com.example</groupId><artifactId>parent</artifactId></parent>
                 <artifactId>core</artifactId>
                 <properties><kotlin.compiler.jvmTarget>21</kotlin.compiler.jvmTarget></properties>
               </project>"#,
        );
        tree.write(
            "app/pom.xml",
            r#"<project>
                 <parent><groupId>com.example</groupId><artifactId>parent</artifactId></parent>
                 <artifactId>app</artifactId>
                 <dependencies>
                   <dependency><groupId>com.example</groupId><artifactId>core</artifactId></dependency>
                   <dependency><groupId>org.junit</groupId><artifactId>junit</artifactId></dependency>
                 </dependencies>
               </project>"#,
        );
        tree.directory("core/src/main/kotlin");
        tree.directory("app/src/main/kotlin");
        tree.directory("app/src/test/kotlin");
        tree.directory("app/target/generated-sources/openapi");
        tree.write(
            "app/.krusty-classpath.txt",
            &classpath_line(&[
                "/m2/kotlin-stdlib.jar",
                "/m2/org/other/core/1.0/core-1.0.jar",
                "/m2/com/example/core/1.0/core-1.0.jar",
            ]),
        );
        tree.write(
            "app/.krusty-classpath-test.txt",
            &classpath_line(&["/m2/kotlin-stdlib.jar", "/m2/junit.jar"]),
        );
        tree.write(
            "core/.krusty-classpath.txt",
            &classpath_line(&["/m2/kotlin-stdlib.jar"]),
        );
        tree
    }

    fn provider_with_effective_pom(tree: &TempTree, effective_pom: &str) -> MavenProvider {
        let provider = MavenProvider::new(tree.root());
        std::fs::write(&provider.effective_pom, effective_pom).unwrap();
        provider
    }

    fn provider(tree: &TempTree) -> MavenProvider {
        let projects = pom_files(tree.root())
            .into_iter()
            .map(|path| std::fs::read_to_string(path).unwrap())
            .collect::<String>();
        provider_with_effective_pom(tree, &format!("<projects>{projects}</projects>"))
    }

    fn probe(tree: &TempTree) -> ProjectModel {
        let runner = FakeRunner::new(vec![
            FakeRunner::stdout(""),
            FakeRunner::stdout(""),
            FakeRunner::stdout(""),
        ]);
        provider(tree).probe(&runner).unwrap()
    }

    #[test]
    fn the_probe_does_not_run_a_build_lifecycle_and_removes_its_output() {
        let tree = reactor_fixture();
        let runner = FakeRunner::new(vec![
            FakeRunner::stdout(""),
            FakeRunner::stdout(""),
            FakeRunner::stdout(""),
        ]);
        let provider = provider(&tree);
        provider.probe(&runner).unwrap();

        let compile = runner.command(0);
        assert_eq!(
            compile.args,
            vec![
                "-B",
                "--quiet",
                "dependency:build-classpath",
                "-Dmdep.outputFile=.krusty-classpath.txt",
                "-Dmdep.includeScope=compile",
            ]
        );
        let test = runner.command(1);
        assert!(test.args.contains(&"-Dmdep.includeScope=test".to_string()));
        let effective = runner.command(2);
        assert!(effective.args.contains(&"help:effective-pom".to_string()));
        assert!(!tree.path("app/.krusty-classpath.txt").exists());
        assert!(!tree.path("core/.krusty-classpath.txt").exists());
        assert!(!provider.effective_pom.exists());
    }

    #[test]
    fn aggregator_modules_produce_no_compilation_unit() {
        let tree = reactor_fixture();
        let model = probe(&tree);
        assert!(model
            .module(&ModuleId::new("com.example:parent", "main"))
            .is_none());
        assert!(model
            .module(&ModuleId::new("com.example:core", "main"))
            .is_some());
    }

    #[test]
    fn a_reactor_dependency_resolves_to_the_sibling_output_not_its_jar() {
        let tree = reactor_fixture();
        let model = probe(&tree);
        let app = model
            .module(&ModuleId::new("com.example:app", "main"))
            .unwrap();
        assert_eq!(
            app.classpath,
            vec![
                PathBuf::from("/m2/kotlin-stdlib.jar"),
                PathBuf::from("/m2/org/other/core/1.0/core-1.0.jar"),
                tree.path("core/target/classes"),
            ]
        );
        assert_eq!(
            app.depends_on,
            vec![ModuleId::new("com.example:core", "main")]
        );
    }

    #[test]
    fn generated_source_roots_are_picked_up_and_marked() {
        let tree = reactor_fixture();
        let model = probe(&tree);
        let app = model
            .module(&ModuleId::new("com.example:app", "main"))
            .unwrap();
        assert_eq!(
            app.source_roots,
            vec![
                SourceRoot::source(tree.path("app/src/main/kotlin")),
                SourceRoot::source(tree.path("app/target/generated-sources/openapi")).generated(),
            ]
        );
    }

    #[test]
    fn test_modules_carry_the_test_classpath_and_the_main_output_as_a_friend_path() {
        let tree = reactor_fixture();
        let model = probe(&tree);
        let tests = model
            .module(&ModuleId::new("com.example:app", "test"))
            .unwrap();
        assert_eq!(
            tests.classpath,
            vec![
                PathBuf::from("/m2/kotlin-stdlib.jar"),
                PathBuf::from("/m2/junit.jar")
            ]
        );
        assert_eq!(tests.friend_paths, vec![tree.path("app/target/classes")]);
        assert!(tests
            .depends_on
            .contains(&ModuleId::new("com.example:app", "main")));

        // `core` has no test sources, so it contributes no test module.
        assert!(model
            .module(&ModuleId::new("com.example:core", "test"))
            .is_none());
    }

    #[test]
    fn the_jvm_target_prefers_the_kotlin_property_over_the_java_one() {
        let tree = reactor_fixture();
        let model = probe(&tree);
        let core = model
            .module(&ModuleId::new("com.example:core", "main"))
            .unwrap();
        assert_eq!(core.jvm_target.as_deref(), Some("21"));
    }

    #[test]
    fn an_active_by_default_profile_contributes_modules_to_the_reactor() {
        let tree = TempTree::new("maven-profile-modules");
        tree.write(
            "pom.xml",
            r#"<project>
                 <groupId>com.example</groupId><artifactId>parent</artifactId><packaging>pom</packaging>
                 <modules><module>core</module></modules>
                 <profiles><profile><id>extra</id>
                   <activation><activeByDefault>true</activeByDefault></activation>
                   <modules><module>extension</module></modules>
                 </profile></profiles>
               </project>"#,
        );
        tree.write(
            "core/pom.xml",
            r#"<project><parent><groupId>com.example</groupId><artifactId>parent</artifactId></parent><artifactId>core</artifactId></project>"#,
        );
        tree.write(
            "extension/pom.xml",
            r#"<project><parent><groupId>com.example</groupId><artifactId>parent</artifactId></parent><artifactId>extension</artifactId></project>"#,
        );
        tree.directory("core/src/main/kotlin");
        tree.directory("extension/src/main/kotlin");
        tree.write("core/.krusty-classpath.txt", "");
        tree.write("extension/.krusty-classpath.txt", "");

        let effective = r#"<projects>
            <project><groupId>com.example</groupId><artifactId>parent</artifactId><packaging>pom</packaging></project>
            <project><groupId>com.example</groupId><artifactId>core</artifactId></project>
            <project><groupId>com.example</groupId><artifactId>extension</artifactId></project>
        </projects>"#;
        let runner = FakeRunner::new(vec![
            FakeRunner::stdout(""),
            FakeRunner::stdout(""),
            FakeRunner::stdout(""),
        ]);
        let model = provider_with_effective_pom(&tree, effective)
            .probe(&runner)
            .unwrap();
        assert!(model
            .module(&ModuleId::new("com.example:extension", "main"))
            .is_some());
    }

    #[test]
    fn a_file_activated_profile_overrides_the_source_directory_and_suppresses_defaults() {
        let tree = TempTree::new("maven-profile-srcdir");
        tree.write(
            "pom.xml",
            r#"<project>
                 <groupId>com.example</groupId><artifactId>app</artifactId>
                 <profiles>
                   <profile><id>generated</id>
                     <activation><file><exists>use-generated.flag</exists></file></activation>
                     <build><sourceDirectory>src/generated/kotlin</sourceDirectory></build>
                   </profile>
                   <profile><id>fallback</id>
                     <activation><activeByDefault>true</activeByDefault></activation>
                   </profile>
                 </profiles>
               </project>"#,
        );
        tree.write("use-generated.flag", "");
        tree.directory("src/generated/kotlin");
        tree.directory("src/main/kotlin");
        tree.write(".krusty-classpath.txt", "");

        let effective = r#"<project>
            <groupId>com.example</groupId><artifactId>app</artifactId>
            <build><sourceDirectory>src/generated/kotlin</sourceDirectory></build>
        </project>"#;
        let runner = FakeRunner::new(vec![
            FakeRunner::stdout(""),
            FakeRunner::stdout(""),
            FakeRunner::stdout(""),
        ]);
        let model = provider_with_effective_pom(&tree, effective)
            .probe(&runner)
            .unwrap();
        let app = model
            .module(&ModuleId::new("com.example:app", "main"))
            .unwrap();
        assert_eq!(
            app.source_roots,
            vec![SourceRoot::source(tree.path("src/generated/kotlin"))]
        );
    }

    #[test]
    fn an_inactive_file_profile_leaves_the_convention_layout() {
        let tree = TempTree::new("maven-profile-inactive");
        tree.write(
            "pom.xml",
            r#"<project>
                 <groupId>com.example</groupId><artifactId>app</artifactId>
                 <profiles><profile><id>generated</id>
                   <activation><file><exists>use-generated.flag</exists></file></activation>
                   <build><sourceDirectory>src/generated/kotlin</sourceDirectory></build>
                 </profile></profiles>
               </project>"#,
        );
        tree.directory("src/main/kotlin");
        tree.write(".krusty-classpath.txt", "");

        let runner = FakeRunner::new(vec![
            FakeRunner::stdout(""),
            FakeRunner::stdout(""),
            FakeRunner::stdout(""),
        ]);
        let model = provider(&tree).probe(&runner).unwrap();
        let app = model
            .module(&ModuleId::new("com.example:app", "main"))
            .unwrap();
        assert_eq!(
            app.source_roots,
            vec![SourceRoot::source(tree.path("src/main/kotlin"))]
        );
    }

    #[test]
    fn a_module_listed_by_two_aggregators_is_collected_once() {
        let tree = TempTree::new("maven-diamond");
        tree.write(
            "pom.xml",
            r#"<project><groupId>com.example</groupId><artifactId>root</artifactId><packaging>pom</packaging>
                 <modules><module>a</module><module>b</module></modules></project>"#,
        );
        tree.write(
            "a/pom.xml",
            r#"<project><groupId>com.example</groupId><artifactId>a</artifactId><packaging>pom</packaging>
                 <modules><module>../shared</module></modules></project>"#,
        );
        tree.write(
            "b/pom.xml",
            r#"<project><groupId>com.example</groupId><artifactId>b</artifactId><packaging>pom</packaging>
                 <modules><module>../shared</module></modules></project>"#,
        );
        tree.write(
            "shared/pom.xml",
            r#"<project><groupId>com.example</groupId><artifactId>shared</artifactId></project>"#,
        );
        tree.directory("shared/src/main/kotlin");
        tree.write("shared/.krusty-classpath.txt", "");

        let runner = FakeRunner::new(vec![
            FakeRunner::stdout(""),
            FakeRunner::stdout(""),
            FakeRunner::stdout(""),
        ]);
        let model = provider(&tree).probe(&runner).unwrap();
        let shared_modules = model
            .modules
            .iter()
            .filter(|module| module.id == Some(ModuleId::new("com.example:shared", "main")))
            .count();
        assert_eq!(shared_modules, 1);
    }

    #[test]
    fn a_failing_build_surfaces_maven_s_own_error_line() {
        let tree = reactor_fixture();
        let runner = FakeRunner::new(vec![FakeRunner::failure(
            1,
            "[ERROR] Failed to execute goal: missing artifact\n",
        )]);
        let error = MavenProvider::new(tree.root()).probe(&runner).unwrap_err();
        assert_eq!(
            error.to_string(),
            "mvn exited with 1: [ERROR] Failed to execute goal: missing artifact"
        );
    }
}
