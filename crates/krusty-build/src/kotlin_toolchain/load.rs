//! Load a project into build modules and compile it.
//!
//! `krusty-toolchain build` reads `module.yaml` and `project.yaml` directly. A Gradle or Maven
//! project is loaded by running that tool and consuming the model it prints. The compiler is the
//! separate `krusty` executable.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};

use crate::compiler::KrustyCli;
use crate::driver::{BuildReport, Driver};
use crate::graph::ModuleGraph;
use crate::model::{Module, ModuleId, ModuleOutput, SourceRoot, SourceRootKind};
use crate::store::ArtifactStore;

use super::catalog::Catalog;
use super::discover::{self, ProjectKind};
use super::maven_model::MavenResolver;
use super::tool::{self, ToolRunner};
use super::yaml::{self, Yaml};

/// Arguments of `krusty-toolchain build`, after the `build` subcommand has been taken off `argv`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildCommand {
    pub directory: PathBuf,
    pub modules: Vec<String>,
    pub platforms: Vec<String>,
    pub variants: Vec<String>,
    /// `krusty` binary used to compile each module.
    pub compiler: PathBuf,
}

/// The compilation units a command selected, in project order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedProject {
    pub root: PathBuf,
    pub modules: Vec<Module>,
}

/// Read the toolchain project selected by `command`.
///
/// Distribution jars are not added here. [`execute`] attaches them before compiling, so a test can
/// assert the graph the project file itself describes.
pub fn load(command: &BuildCommand) -> Result<LoadedProject, String> {
    load_using(command, &tool::ProcessRunner)
}

pub(super) fn load_using(
    command: &BuildCommand,
    runner: &dyn ToolRunner,
) -> Result<LoadedProject, String> {
    validate_flags(command)?;
    let start = absolute(&command.directory)?;
    let kind = discover::discover(&start).ok_or_else(|| {
        format!(
            "no Kotlin Toolchain, Gradle, or Maven project found at {} or its parents (looked for module.yaml, project.yaml, a Gradle build, or pom.xml)",
            start.display()
        )
    })?;
    match kind {
        ProjectKind::Jps(root) => Err(discover::jps_message(&root)),
        ProjectKind::Gradle(root) => {
            super::gradle_model::load_project(&root, &command.modules, runner)
        }
        ProjectKind::Maven(root) => {
            super::maven_model::load_project(&root, &command.modules, runner)
        }
        ProjectKind::Toolchain(root) => load_toolchain(&root, command, runner),
    }
}

fn load_toolchain(
    root: &Path,
    command: &BuildCommand,
    runner: &dyn ToolRunner,
) -> Result<LoadedProject, String> {
    let declared = read_project(root)?;
    let selection = select(&declared, &command.modules)?;
    let mut resolver = MavenResolver::new(runner, root);
    let modules = compilation_units(root, &declared, &selection, &mut resolver)?;
    if modules.is_empty() {
        return Err(format!(
            "{} has no Kotlin sources to compile",
            root.display()
        ));
    }
    Ok(LoadedProject {
        root: root.to_path_buf(),
        modules,
    })
}

/// Compile the project `command` describes.
pub fn execute(command: &BuildCommand) -> Result<BuildReport, String> {
    if !command.compiler.is_file() {
        return Err(format!(
            "compiler binary not found: {}",
            command.compiler.display()
        ));
    }
    let mut loaded = load(command)?;
    attach_distribution(&mut loaded.modules)?;
    let mut graph = ModuleGraph::new();
    for module in loaded.modules {
        graph.insert(module).map_err(|error| error.to_string())?;
    }
    let store_dir = loaded.root.join("build/krusty/store");
    let scratch = loaded.root.join("build/krusty/scratch");
    let store = ArtifactStore::open(&store_dir)
        .map_err(|error| format!("cannot open {}: {error}", store_dir.display()))?;
    let environment = KrustyCli::new(&command.compiler, scratch);
    Driver::new(environment, store)
        .build(&graph)
        .map_err(|error| error.to_string())
}

fn validate_flags(command: &BuildCommand) -> Result<(), String> {
    let unknown_platforms: Vec<&str> = command
        .platforms
        .iter()
        .map(String::as_str)
        .filter(|platform| *platform != "jvm")
        .collect();
    if !unknown_platforms.is_empty() {
        return Err(format!(
            "krusty-toolchain build compiles the JVM. Unsupported platform(s): {}",
            unknown_platforms.join(", ")
        ));
    }
    let unknown_variants: Vec<&str> = command
        .variants
        .iter()
        .map(String::as_str)
        .filter(|variant| !matches!(*variant, "debug" | "release"))
        .collect();
    if !unknown_variants.is_empty() {
        return Err(format!(
            "invalid variant(s): {}. Supported values: debug, release",
            unknown_variants.join(", ")
        ));
    }
    Ok(())
}

fn attach_distribution(modules: &mut [Module]) -> Result<(), String> {
    let stdlib = krusty::jvm::kotlin_stdlib_jar().ok_or_else(|| {
        "cannot locate kotlin-stdlib.jar; krusty-toolchain build adds the standard library the Kotlin distribution provides"
            .to_string()
    })?;
    require_absolute("kotlin-stdlib.jar", &stdlib)?;
    let needs_test = modules.iter().any(is_test_module);
    let test_jar = if needs_test {
        let jar = krusty::jvm::kotlin_dist_jar("kotlin-test.jar").ok_or_else(|| {
            "cannot locate kotlin-test.jar; a test module needs the Kotlin test library".to_string()
        })?;
        require_absolute("kotlin-test.jar", &jar)?;
        Some(jar)
    } else {
        None
    };
    for module in modules {
        push_jar(&mut module.classpath, &stdlib);
        if is_test_module(module) {
            if let Some(jar) = &test_jar {
                push_jar(&mut module.classpath, jar);
            }
        }
    }
    Ok(())
}

fn require_absolute(name: &str, path: &Path) -> Result<(), String> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(format!(
            "{name} resolved to a relative path {}",
            path.display()
        ))
    }
}

fn push_jar(classpath: &mut Vec<PathBuf>, jar: &Path) {
    if !classpath.iter().any(|entry| entry == jar) {
        classpath.push(jar.to_path_buf());
    }
}

fn is_test_module(module: &Module) -> bool {
    module
        .id
        .as_ref()
        .is_some_and(|id| id.as_str().ends_with(":test"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Product {
    JvmApp,
    JvmLib,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    Amper,
    MavenLike,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    All,
    CompileOnly,
    RuntimeOnly,
}

#[derive(Clone, Debug)]
enum DepTarget {
    Module(usize),
    Maven(String),
}

#[derive(Clone, Debug)]
struct Dep {
    target: DepTarget,
    exported: bool,
    scope: Scope,
}

#[derive(Clone, Debug)]
struct Declared {
    display_name: String,
    directory: PathBuf,
    file: PathBuf,
    dependencies: Vec<Dep>,
    test_dependencies: Vec<Dep>,
    main: SourceFiles,
    test: SourceFiles,
}

#[derive(Clone, Debug, Default)]
struct SourceFiles {
    roots: Vec<PathBuf>,
    resources: Vec<PathBuf>,
    java: Vec<PathBuf>,
    has_kotlin: bool,
}

impl SourceFiles {
    fn compiles(&self) -> bool {
        self.has_kotlin || !self.java.is_empty()
    }
}

fn read_project(root: &Path) -> Result<Vec<Declared>, String> {
    let catalog = Catalog::load(root)?;
    let project_file = root.join("project.yaml");
    let root_module = root.join("module.yaml");
    let relatives = if project_file.is_file() {
        let mut listed = module_list(root, &project_file)?;
        if root_module.is_file() && !listed.iter().any(|path| path.is_empty()) {
            listed.insert(0, String::new());
        }
        listed
    } else if root_module.is_file() {
        vec![String::new()]
    } else {
        return Err(format!(
            "{} was selected as a toolchain project but has no module.yaml or project.yaml",
            root.display()
        ));
    };
    let mut seen = BTreeSet::new();
    let mut skeletons = Vec::new();
    for relative in relatives {
        if !seen.insert(relative.clone()) {
            return Err(format!("project.yaml lists '{relative}' more than once"));
        }
        let directory = if relative.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&relative)
        };
        let file = directory.join("module.yaml");
        if !file.is_file() {
            return Err(format!("missing {}", file.display()));
        }
        skeletons.push((relative, directory, file));
    }
    let mut parsed = Vec::with_capacity(skeletons.len());
    for (relative, directory, file) in &skeletons {
        parsed.push(parse_module_file(
            root, relative, directory, file, &catalog,
        )?);
    }
    let index_of = |relative: &str| {
        skeletons
            .iter()
            .position(|(candidate, _, _)| candidate == relative)
    };
    let mut declared = Vec::with_capacity(parsed.len());
    for (index, (relative, directory, file)) in skeletons.iter().enumerate() {
        let display_name = if relative.is_empty() {
            root.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "root".to_string())
        } else {
            relative.clone()
        };
        if declared
            .iter()
            .any(|module: &Declared| module.display_name == display_name)
        {
            return Err(format!(
                "module name '{display_name}' is used by more than one module"
            ));
        }
        let spec = &parsed[index];
        let dependencies = resolve_deps(&spec.dependencies, file, directory, root, index_of)?;
        let test_dependencies =
            resolve_deps(&spec.test_dependencies, file, directory, root, index_of)?;
        let (main, test) = source_sets(directory, spec.layout, file)?;
        declared.push(Declared {
            display_name,
            directory: directory.clone(),
            file: file.clone(),
            dependencies,
            test_dependencies,
            main,
            test,
        });
    }
    Ok(declared)
}

struct ModuleSpec {
    layout: Layout,
    dependencies: Vec<DepSpec>,
    test_dependencies: Vec<DepSpec>,
}

struct DepSpec {
    notation: String,
    exported: bool,
    scope: Scope,
    maven: bool,
}

fn parse_module_file(
    _root: &Path,
    _relative: &str,
    _directory: &Path,
    file: &Path,
    catalog: &Catalog,
) -> Result<ModuleSpec, String> {
    let document = read_yaml(file)?;
    let entries = document
        .as_map()
        .ok_or_else(|| format!("{}: expected a mapping", file.display()))?;
    let mut product = None;
    let mut layout = Layout::Amper;
    let mut dependencies = Vec::new();
    let mut test_dependencies = Vec::new();
    for (key, value) in entries {
        match key.as_str() {
            "product" => product = Some(parse_product(file, value)?),
            "layout" => layout = parse_layout(file, value)?,
            "dependencies" => dependencies = parse_dep_list(file, value, catalog)?,
            "test-dependencies" => test_dependencies = parse_dep_list(file, value, catalog)?,
            "description" => {}
            other => {
                return Err(format!("{}: unsupported key '{other}'", file.display()));
            }
        }
    }
    let product = product.ok_or_else(|| format!("{}: missing product", file.display()))?;
    if layout == Layout::MavenLike && !matches!(product, Product::JvmApp | Product::JvmLib) {
        return Err(format!(
            "{}: layout maven-like is only supported for jvm/app and jvm/lib",
            file.display()
        ));
    }
    Ok(ModuleSpec {
        layout,
        dependencies,
        test_dependencies,
    })
}

fn parse_product(file: &Path, value: &Yaml) -> Result<Product, String> {
    let type_name = match value {
        Yaml::Scalar(name) => name.clone(),
        Yaml::Map(entries) => {
            let mut type_name = None;
            for (key, nested) in entries {
                if key != "type" {
                    return Err(format!(
                        "{}: unsupported product key '{key}'",
                        file.display()
                    ));
                }
                type_name = Some(scalar(file, "product.type", nested)?);
            }
            type_name.ok_or_else(|| format!("{}: product is missing type", file.display()))?
        }
        Yaml::Seq(_) => {
            return Err(format!("{}: product must be a type", file.display()));
        }
    };
    match type_name.as_str() {
        "jvm/app" => Ok(Product::JvmApp),
        "jvm/lib" => Ok(Product::JvmLib),
        other => Err(format!(
            "{file}: unsupported product type '{other}'; krusty-toolchain build compiles jvm/app and jvm/lib",
            file = file.display()
        )),
    }
}

fn parse_layout(file: &Path, value: &Yaml) -> Result<Layout, String> {
    match scalar(file, "layout", value)?.as_str() {
        "amper" => Ok(Layout::Amper),
        "maven-like" => Ok(Layout::MavenLike),
        other => Err(format!("{}: unsupported layout '{other}'", file.display())),
    }
}

fn parse_dep_list(file: &Path, value: &Yaml, catalog: &Catalog) -> Result<Vec<DepSpec>, String> {
    let Some(items) = value.as_seq() else {
        if matches!(value, Yaml::Scalar(text) if text.is_empty()) {
            return Ok(Vec::new());
        }
        return Err(format!("{}: dependencies must be a list", file.display()));
    };
    items
        .iter()
        .map(|item| parse_dep(file, item, catalog))
        .collect()
}

fn parse_dep(file: &Path, item: &Yaml, catalog: &Catalog) -> Result<DepSpec, String> {
    match item {
        Yaml::Scalar(notation) => dep_spec(file, catalog, notation, false, Scope::All),
        Yaml::Map(entries) if entries.len() == 1 => {
            let (notation, value) = &entries[0];
            let (exported, scope) = match value {
                Yaml::Scalar(attribute) if attribute.is_empty() => (false, Scope::All),
                Yaml::Scalar(attribute) => short_attribute(file, attribute)?,
                Yaml::Map(attributes) => full_attributes(file, attributes)?,
                Yaml::Seq(_) => {
                    return Err(format!(
                        "{}: dependency '{notation}' has no attributes",
                        file.display()
                    ));
                }
            };
            dep_spec(file, catalog, notation, exported, scope)
        }
        _ => Err(format!(
            "{}: a dependency must be a module path or a Maven coordinate",
            file.display()
        )),
    }
}

fn short_attribute(file: &Path, attribute: &str) -> Result<(bool, Scope), String> {
    match attribute {
        "exported" => Ok((true, Scope::All)),
        "all" => Ok((false, Scope::All)),
        "compile-only" => Ok((false, Scope::CompileOnly)),
        "runtime-only" => Ok((false, Scope::RuntimeOnly)),
        other => Err(format!(
            "{}: unsupported dependency attribute '{other}'",
            file.display()
        )),
    }
}

fn full_attributes(file: &Path, attributes: &[(String, Yaml)]) -> Result<(bool, Scope), String> {
    let mut exported = false;
    let mut scope = Scope::All;
    for (key, value) in attributes {
        match key.as_str() {
            "exported" => {
                exported = match scalar(file, "exported", value)?.as_str() {
                    "true" => true,
                    "false" => false,
                    other => {
                        return Err(format!(
                            "{}: exported must be true or false, found '{other}'",
                            file.display()
                        ));
                    }
                };
            }
            "scope" => scope = parse_scope(file, &scalar(file, "scope", value)?)?,
            other => {
                return Err(format!(
                    "{}: unsupported dependency attribute '{other}'",
                    file.display()
                ));
            }
        }
    }
    Ok((exported, scope))
}

fn parse_scope(file: &Path, value: &str) -> Result<Scope, String> {
    match value {
        "all" => Ok(Scope::All),
        "compile-only" => Ok(Scope::CompileOnly),
        "runtime-only" => Ok(Scope::RuntimeOnly),
        other => Err(format!(
            "{}: unsupported dependency scope '{other}'",
            file.display()
        )),
    }
}

fn dep_spec(
    file: &Path,
    catalog: &Catalog,
    notation: &str,
    exported: bool,
    scope: Scope,
) -> Result<DepSpec, String> {
    let resolved = if notation.starts_with('$') {
        Some(catalog.resolve(file, notation)?)
    } else {
        None
    };
    let notation = resolved.as_deref().unwrap_or(notation);
    if notation.starts_with("bom:")
        || notation.starts_with("swiftPackage:")
        || notation.starts_with("localSwiftPackage:")
    {
        return Err(format!(
            "{file}: dependency '{notation}' is not a project module",
            file = file.display()
        ));
    }
    if notation.starts_with("//") || notation.starts_with('.') {
        return Ok(DepSpec {
            notation: notation.to_string(),
            exported,
            scope,
            maven: false,
        });
    }
    if super::maven_model::is_maven_coordinate(notation) {
        return Ok(DepSpec {
            notation: notation.to_string(),
            exported,
            scope,
            maven: true,
        });
    }
    Err(format!(
        "{file}: dependency '{notation}' is not a Maven coordinate",
        file = file.display()
    ))
}

fn resolve_deps(
    specs: &[DepSpec],
    file: &Path,
    directory: &Path,
    root: &Path,
    index_of: impl Fn(&str) -> Option<usize>,
) -> Result<Vec<Dep>, String> {
    let mut deps = Vec::new();
    for spec in specs {
        if spec.maven {
            deps.push(Dep {
                target: DepTarget::Maven(spec.notation.clone()),
                exported: spec.exported,
                scope: spec.scope,
            });
            continue;
        }
        let relative = module_reference(root, directory, &spec.notation)
            .map_err(|message| format!("{}: {message}", file.display()))?;
        let Some(target) = index_of(&relative) else {
            return Err(format!(
                "{}: dependency '{}' does not match a module in this project",
                file.display(),
                spec.notation
            ));
        };
        deps.push(Dep {
            target: DepTarget::Module(target),
            exported: spec.exported,
            scope: spec.scope,
        });
    }
    Ok(deps)
}

fn module_index(dep: &Dep) -> Option<usize> {
    match dep.target {
        DepTarget::Module(index) => Some(index),
        DepTarget::Maven(_) => None,
    }
}

fn module_reference(root: &Path, from: &Path, notation: &str) -> Result<String, String> {
    let relative = if let Some(rest) = notation.strip_prefix("//") {
        rest.trim_start_matches('/')
    } else {
        let joined = from.join(notation);
        let normalized = lexical_normalize(&joined);
        let Ok(stripped) = normalized.strip_prefix(root) else {
            return Err(format!("dependency '{notation}' escapes the project"));
        };
        return path_key(stripped);
    };
    if relative.is_empty() || relative == "." {
        return Ok(String::new());
    }
    path_key(Path::new(relative))
}

fn path_key(path: &Path) -> Result<String, String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            _ => {
                return Err(format!(
                    "module path '{}' leaves the project",
                    path.display()
                ));
            }
        }
    }
    Ok(parts.join("/"))
}

fn module_list(root: &Path, file: &Path) -> Result<Vec<String>, String> {
    let document = read_yaml(file)?;
    let entries = document
        .as_map()
        .ok_or_else(|| format!("{}: expected a mapping", file.display()))?;
    let mut modules = None;
    for (key, value) in entries {
        match key.as_str() {
            "modules" => modules = Some(value),
            other => {
                return Err(format!("{}: unsupported key '{other}'", file.display()));
            }
        }
    }
    let Some(modules) = modules else {
        return Err(format!("{}: missing modules", file.display()));
    };
    let Some(items) = modules.as_seq() else {
        return Err(format!("{}: modules must be a list", file.display()));
    };
    let mut paths = Vec::new();
    for item in items {
        let pattern = scalar(file, "modules", item)?;
        paths.extend(expand_pattern(root, file, &pattern)?);
    }
    Ok(paths)
}

fn expand_pattern(root: &Path, file: &Path, pattern: &str) -> Result<Vec<String>, String> {
    let pattern = pattern.trim_start_matches("//").trim_matches('/');
    if pattern.is_empty() || pattern == "." {
        return Ok(vec![String::new()]);
    }
    if !pattern.contains('*') {
        return path_key(Path::new(pattern)).map(|key| vec![key]);
    }
    let segments: Vec<&str> = pattern.split('/').collect();
    if segments.iter().filter(|segment| **segment == "*").count() != 1
        || segments
            .iter()
            .any(|segment| segment.contains('*') && *segment != "*")
    {
        return Err(format!(
            "{}: module pattern '{pattern}' is not a single-segment glob",
            file.display()
        ));
    }
    let Some(star) = segments.iter().position(|segment| *segment == "*") else {
        return Err(format!(
            "{}: module pattern '{pattern}' is not a single-segment glob",
            file.display()
        ));
    };
    let mut directories = vec![root.to_path_buf()];
    for (index, segment) in segments.iter().enumerate() {
        if index == star {
            let mut next = Vec::new();
            for directory in directories {
                let entries = std::fs::read_dir(&directory)
                    .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
                for entry in entries {
                    let entry = entry.map_err(|error| {
                        format!(
                            "cannot read an entry under {}: {error}",
                            directory.display()
                        )
                    })?;
                    if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                        next.push(entry.path());
                    }
                }
            }
            next.sort();
            directories = next;
        } else {
            for directory in &mut directories {
                directory.push(segment);
            }
        }
    }
    let mut matched = Vec::new();
    for directory in directories {
        if directory.join("module.yaml").is_file() {
            let key = path_key(directory.strip_prefix(root).map_err(|_| {
                format!(
                    "{}: glob result {} is outside the project",
                    file.display(),
                    directory.display()
                )
            })?)?;
            matched.push(key);
        }
    }
    if matched.is_empty() {
        return Err(format!(
            "{}: module pattern '{pattern}' matched no modules",
            file.display()
        ));
    }
    matched.sort();
    Ok(matched)
}

fn source_sets(
    directory: &Path,
    layout: Layout,
    file: &Path,
) -> Result<(SourceFiles, SourceFiles), String> {
    let (main_roots, main_resources, test_roots, test_resources) = match layout {
        Layout::Amper => (
            vec![directory.join("src")],
            vec![directory.join("resources")],
            vec![directory.join("test")],
            vec![directory.join("testResources")],
        ),
        Layout::MavenLike => (
            vec![
                directory.join("src/main/kotlin"),
                directory.join("src/main/java"),
            ],
            vec![directory.join("src/main/resources")],
            vec![
                directory.join("src/test/kotlin"),
                directory.join("src/test/java"),
            ],
            vec![directory.join("src/test/resources")],
        ),
    };
    Ok((
        classify(file, &main_roots, &main_resources)?,
        classify(file, &test_roots, &test_resources)?,
    ))
}

fn classify(file: &Path, roots: &[PathBuf], resources: &[PathBuf]) -> Result<SourceFiles, String> {
    let mut files = SourceFiles::default();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        files.roots.push(root.clone());
        scan_tree(file, root, &mut files)?;
    }
    for resource in resources {
        if resource.is_dir() {
            files.resources.push(resource.clone());
        }
    }
    files.java.sort();
    Ok(files)
}

pub(super) struct FoundSources {
    pub roots: Vec<PathBuf>,
    pub java: Vec<PathBuf>,
    pub has_kotlin: bool,
}

pub(super) fn scan_compilation_roots(
    label: &Path,
    roots: &[PathBuf],
) -> Result<FoundSources, String> {
    let mut files = SourceFiles::default();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        files.roots.push(root.clone());
        scan_tree(label, root, &mut files)?;
    }
    files.java.sort();
    Ok(FoundSources {
        roots: files.roots,
        java: files.java,
        has_kotlin: files.has_kotlin,
    })
}

fn scan_tree(file: &Path, directory: &Path, files: &mut SourceFiles) -> Result<(), String> {
    let entries = std::fs::read_dir(directory).map_err(|error| {
        format!(
            "{}: cannot read {}: {error}",
            file.display(),
            directory.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "{}: cannot read an entry under {}: {error}",
                file.display(),
                directory.display()
            )
        })?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|error| {
            format!(
                "{}: cannot stat {}: {error}",
                file.display(),
                path.display()
            )
        })?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            scan_tree(file, &path, files)?;
        } else if kind.is_file() {
            match path.extension().and_then(|extension| extension.to_str()) {
                Some("kt") => files.has_kotlin = true,
                Some("kts") => {
                    return Err(format!(
                        "{}: Kotlin scripts are not compiled ({})",
                        file.display(),
                        path.display()
                    ));
                }
                Some("java") => files.java.push(path),
                _ => {}
            }
        }
    }
    Ok(())
}

struct Selection {
    /// Modules named by the command, or every module when the command names none.
    direct: BTreeSet<usize>,
    /// `direct` plus the modules they need on the compile classpath.
    included: BTreeSet<usize>,
}

fn select(declared: &[Declared], names: &[String]) -> Result<Selection, String> {
    let mut direct = BTreeSet::new();
    if names.is_empty() {
        direct.extend(0..declared.len());
    } else {
        for name in names {
            let Some(index) = declared
                .iter()
                .position(|module| module.display_name == *name)
            else {
                let mut available: Vec<&str> = declared
                    .iter()
                    .map(|module| module.display_name.as_str())
                    .collect();
                available.sort_unstable();
                return Err(format!(
                    "no module named '{name}'.\nAvailable modules:\n{}",
                    available
                        .iter()
                        .map(|module| format!("- {module}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            };
            direct.insert(index);
        }
    }
    let mut included = BTreeSet::new();
    let mut queue: VecDeque<usize> = direct.iter().copied().collect();
    while let Some(index) = queue.pop_front() {
        if !included.insert(index) {
            continue;
        }
        for dependency in &declared[index].dependencies {
            if dependency.scope != Scope::RuntimeOnly {
                if let Some(target) = module_index(dependency) {
                    queue.push_back(target);
                }
            }
        }
        if direct.contains(&index) {
            for dependency in &declared[index].test_dependencies {
                if dependency.scope != Scope::RuntimeOnly {
                    if let Some(target) = module_index(dependency) {
                        queue.push_back(target);
                    }
                }
            }
        }
    }
    Ok(Selection { direct, included })
}

fn compilation_units(
    root: &Path,
    declared: &[Declared],
    selection: &Selection,
    resolver: &mut MavenResolver<'_>,
) -> Result<Vec<Module>, String> {
    let mut units = Vec::new();
    for (index, module) in declared.iter().enumerate() {
        if !selection.included.contains(&index) {
            continue;
        }
        let selected = selection.direct.contains(&index);
        if selected && !module.main.compiles() && !module.test.compiles() {
            return Err(format!(
                "{}: module '{}' has no Kotlin sources to compile",
                module.file.display(),
                module.display_name
            ));
        }
        if module.main.compiles() {
            units.push(unit(root, declared, index, false, resolver)?);
        } else if declared.iter().enumerate().any(|(other, candidate)| {
            selection.included.contains(&other)
                && other != index
                && depends_on_for_compile(candidate, index)
        }) {
            return Err(format!(
                "{}: module '{}' has no Kotlin sources to compile",
                module.file.display(),
                module.display_name
            ));
        }
        if selected && module.test.compiles() {
            units.push(unit(root, declared, index, true, resolver)?);
        }
    }
    Ok(units)
}

fn depends_on_for_compile(module: &Declared, target: usize) -> bool {
    let points_at = |dependency: &Dep| {
        module_index(dependency) == Some(target) && dependency.scope != Scope::RuntimeOnly
    };
    module.dependencies.iter().any(points_at) || module.test_dependencies.iter().any(points_at)
}

fn unit(
    root: &Path,
    declared: &[Declared],
    index: usize,
    test: bool,
    resolver: &mut MavenResolver<'_>,
) -> Result<Module, String> {
    let module = &declared[index];
    let sources = if test { &module.test } else { &module.main };
    let suffix = if test { "test" } else { "main" };
    let mut built = Module::new(
        ModuleId::new(format!("{}:{suffix}", module.display_name)),
        &module.directory,
    );
    built.display_name = module.display_name.clone();
    built.module_name = Some(module.display_name.clone());
    built.source_roots = sources
        .roots
        .iter()
        .map(|path| SourceRoot {
            path: path.clone(),
            kind: if test {
                SourceRootKind::Test
            } else {
                SourceRootKind::Main
            },
            generated: false,
        })
        .collect();
    built.resources = sources.resources.clone();
    built.java_sources = sources.java.clone();
    built.outputs = vec![ModuleOutput::ClassDirectory(output_path(
        root,
        &module.display_name,
        test,
    ))];
    let name = module.display_name.clone();
    built.depends_on = if test {
        let mut ids = Vec::new();
        if module.main.compiles() {
            ids.push(ModuleId::new(format!("{name}:main")));
            built.friend_paths = vec![output_path(root, &name, false)];
        }
        ids.extend(closure(&name, declared, &declared[index].dependencies)?);
        ids.extend(closure(
            &name,
            declared,
            &declared[index].test_dependencies,
        )?);
        dedup(ids)
    } else {
        closure(&name, declared, &declared[index].dependencies)?
    };
    attach_maven_jars(
        &mut built,
        declared,
        &declared[index].dependencies,
        resolver,
    )?;
    if test {
        attach_maven_jars(
            &mut built,
            declared,
            &declared[index].test_dependencies,
            resolver,
        )?;
    }
    Ok(built)
}

fn attach_maven_jars(
    built: &mut Module,
    declared: &[Declared],
    edges: &[Dep],
    resolver: &mut MavenResolver<'_>,
) -> Result<(), String> {
    for edge in edges {
        if edge.scope == Scope::RuntimeOnly {
            continue;
        }
        if let DepTarget::Maven(coordinate) = &edge.target {
            for jar in resolver.jars(coordinate)? {
                push_jar(&mut built.classpath, &jar);
            }
        }
    }
    for id in built.depends_on.clone() {
        let Some(name) = id.as_str().strip_suffix(":main") else {
            continue;
        };
        let Some(module) = declared.iter().find(|module| module.display_name == name) else {
            continue;
        };
        for edge in &module.dependencies {
            if !edge.exported || edge.scope == Scope::RuntimeOnly {
                continue;
            }
            if let DepTarget::Maven(coordinate) = &edge.target {
                for jar in resolver.jars(coordinate)? {
                    push_jar(&mut built.classpath, &jar);
                }
            }
        }
    }
    Ok(())
}

fn closure(owner: &str, declared: &[Declared], edges: &[Dep]) -> Result<Vec<ModuleId>, String> {
    let mut ids = Vec::new();
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::new();
    for edge in edges {
        if edge.scope != Scope::RuntimeOnly {
            if let Some(target) = module_index(edge) {
                queue.push_back(target);
            }
        }
    }
    while let Some(target) = queue.pop_front() {
        if !seen.insert(target) {
            continue;
        }
        let module = &declared[target];
        if !module.main.compiles() {
            return Err(format!(
                "module '{owner}' depends on '{}', which has no Kotlin sources",
                module.display_name
            ));
        }
        ids.push(ModuleId::new(format!("{}:main", module.display_name)));
        for child in &module.dependencies {
            if child.exported && child.scope != Scope::RuntimeOnly {
                if let Some(target) = module_index(child) {
                    queue.push_back(target);
                }
            }
        }
    }
    Ok(ids)
}

fn dedup(ids: Vec<ModuleId>) -> Vec<ModuleId> {
    let mut seen = BTreeSet::new();
    ids.into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

fn output_path(root: &Path, name: &str, test: bool) -> PathBuf {
    root.join("build/krusty/modules")
        .join(name)
        .join(if test { "test-classes" } else { "classes" })
}

pub(super) fn select_reported(
    modules: Vec<Module>,
    names: &[String],
) -> Result<Vec<Module>, String> {
    if names.is_empty() {
        return Ok(modules);
    }
    let mut wanted = BTreeSet::new();
    for name in names {
        let hits: Vec<usize> = modules
            .iter()
            .enumerate()
            .filter(|(_, module)| module_matches(module, name))
            .map(|(index, _)| index)
            .collect();
        if hits.is_empty() {
            let mut available = Vec::new();
            for module in &modules {
                available.push(module.display_name.clone());
                if let Some(id) = &module.id {
                    if id.as_str() != module.display_name {
                        available.push(id.as_str().to_string());
                    }
                }
            }
            available.sort();
            available.dedup();
            return Err(format!(
                "no module named '{name}'.\nAvailable modules:\n{}",
                available
                    .iter()
                    .map(|module| format!("- {module}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        wanted.extend(hits);
    }
    let mut included = wanted.clone();
    let mut queue: VecDeque<usize> = wanted.into_iter().collect();
    while let Some(index) = queue.pop_front() {
        for dependency in &modules[index].depends_on {
            let Some(dep_index) = modules.iter().position(|module| {
                module
                    .id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == dependency.as_str())
            }) else {
                return Err(format!(
                    "module '{}' depends on '{}', which is not part of this build",
                    modules[index]
                        .id
                        .as_ref()
                        .map(ModuleId::as_str)
                        .unwrap_or(""),
                    dependency.as_str()
                ));
            };
            if included.insert(dep_index) {
                queue.push_back(dep_index);
            }
        }
    }
    Ok(modules
        .into_iter()
        .enumerate()
        .filter(|(index, _)| included.contains(index))
        .map(|(_, module)| module)
        .collect())
}

fn module_matches(module: &Module, name: &str) -> bool {
    module.display_name == name || module.id.as_ref().is_some_and(|id| id.as_str() == name)
}

fn read_yaml(path: &Path) -> Result<Yaml, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    yaml::parse(&text).map_err(|error| format!("{}: {error}", path.display()))
}

fn scalar(file: &Path, field: &str, value: &Yaml) -> Result<String, String> {
    match value {
        Yaml::Scalar(text) => Ok(text.clone()),
        _ => Err(format!("{}: {field} must be a string", file.display())),
    }
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(lexical_normalize(path))
    } else {
        let current = std::env::current_dir()
            .map_err(|error| format!("cannot determine the working directory: {error}"))?;
        Ok(lexical_normalize(&current.join(path)))
    }
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    normalized
}

#[cfg(test)]
mod coverage;

#[cfg(test)]
mod external_tools;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Temp(PathBuf);

    impl Temp {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "krusty-toolchain-{label}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("temp");
            Self(path)
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("parent");
            }
            std::fs::write(path, contents).expect("write");
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn command(directory: &Path) -> BuildCommand {
        BuildCommand {
            directory: directory.to_path_buf(),
            modules: Vec::new(),
            platforms: Vec::new(),
            variants: Vec::new(),
            compiler: PathBuf::new(),
        }
    }

    fn ids(modules: &[Module]) -> Vec<String> {
        modules
            .iter()
            .map(|module| module.id.as_ref().unwrap().as_str().to_string())
            .collect()
    }

    fn deps(module: &Module) -> Vec<String> {
        module
            .depends_on
            .iter()
            .map(|id| id.as_str().to_string())
            .collect()
    }

    #[test]
    fn a_single_jvm_app_maps_main_and_test_sources() {
        let tree = Temp::new("single");
        tree.write("module.yaml", "product: jvm/app\n");
        tree.write("src/main.kt", "fun main() {}\n");
        tree.write("test/MainTest.kt", "fun test() {}\n");
        tree.write("resources/logback.xml", "<configuration/>\n");

        let loaded = load(&command(&tree.0)).expect("load");
        let name = tree.0.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            ids(&loaded.modules),
            vec![format!("{name}:main"), format!("{name}:test")]
        );
        assert!(loaded.modules[0].source_roots[0].path.ends_with("src"));
        assert_eq!(loaded.modules[0].source_roots[0].kind, SourceRootKind::Main);
        assert!(loaded.modules[0].resources[0].ends_with("resources"));
        assert!(loaded.modules[0].classpath.is_empty());
        assert_eq!(loaded.modules[1].source_roots[0].kind, SourceRootKind::Test);
        assert_eq!(deps(&loaded.modules[1]), vec![format!("{name}:main")]);
        assert_eq!(
            loaded.modules[1].friend_paths,
            vec![loaded.modules[0].outputs[0].path().to_path_buf()]
        );
    }

    #[test]
    fn maven_like_layout_uses_maven_source_directories() {
        let tree = Temp::new("maven");
        tree.write(
            "module.yaml",
            "product:\n  type: jvm/lib\nlayout: maven-like\n",
        );
        tree.write("src/main/kotlin/Lib.kt", "class Lib\n");
        tree.write("src/main/java/Legacy.java", "class Legacy {}\n");
        tree.write("src/test/kotlin/LibTest.kt", "class LibTest\n");

        let loaded = load(&command(&tree.0)).expect("load");
        assert!(loaded.modules[0].source_roots.iter().any(|root| {
            root.path.ends_with("src/main/kotlin") && root.kind == SourceRootKind::Main
        }));
        assert!(loaded.modules[0]
            .java_sources
            .iter()
            .any(|path| path.ends_with("Legacy.java")));
        assert_eq!(loaded.modules[1].source_roots[0].kind, SourceRootKind::Test);
    }

    #[test]
    fn exported_dependencies_are_transitive_and_runtime_only_dependencies_are_not() {
        let tree = Temp::new("graph");
        tree.write(
            "project.yaml",
            "modules:\n  - app\n  - lib\n  - core\n  - runtime\n",
        );
        tree.write(
            "app/module.yaml",
            "product: jvm/app\ndependencies:\n  - //lib\n  - //runtime: runtime-only\ntest-dependencies:\n  - //core\n",
        );
        tree.write("app/src/main.kt", "fun main() {}\n");
        tree.write("app/test/AppTest.kt", "fun check() {}\n");
        tree.write(
            "lib/module.yaml",
            "product: jvm/lib\ndependencies:\n  - //core:\n      exported: true\n",
        );
        tree.write("lib/src/Lib.kt", "class Lib\n");
        tree.write("core/module.yaml", "product: jvm/lib\n");
        tree.write("core/src/Core.kt", "class Core\n");
        tree.write("runtime/module.yaml", "product: jvm/lib\n");
        tree.write("runtime/src/Runtime.kt", "class Runtime\n");

        let loaded = load(&command(&tree.0)).expect("load");
        let app_main = loaded
            .modules
            .iter()
            .find(|module| module.id.as_ref().unwrap().as_str() == "app:main")
            .unwrap();
        let app_test = loaded
            .modules
            .iter()
            .find(|module| module.id.as_ref().unwrap().as_str() == "app:test")
            .unwrap();
        assert_eq!(
            deps(app_main),
            vec!["lib:main".to_string(), "core:main".to_string()]
        );
        assert_eq!(
            deps(app_test),
            vec![
                "app:main".to_string(),
                "lib:main".to_string(),
                "core:main".to_string()
            ]
        );
        assert!(ids(&loaded.modules).iter().any(|id| id == "runtime:main"));

        let mut graph = ModuleGraph::new();
        for module in &loaded.modules {
            graph.insert(module.clone()).expect("insert");
        }
        graph.build_order().expect("acyclic");
    }

    #[test]
    fn a_module_filter_builds_that_module_and_its_dependencies_without_their_tests() {
        let tree = Temp::new("filter");
        tree.write("project.yaml", "modules:\n  - app\n  - lib\n");
        tree.write(
            "app/module.yaml",
            "product: jvm/app\ndependencies:\n  - //lib\n",
        );
        tree.write("app/src/main.kt", "fun main() {}\n");
        tree.write("app/test/AppTest.kt", "fun check() {}\n");
        tree.write("lib/module.yaml", "product: jvm/lib\n");
        tree.write("lib/src/Lib.kt", "class Lib\n");
        tree.write("lib/test/LibTest.kt", "fun libTest() {}\n");

        let mut request = command(&tree.0);
        request.modules = vec!["app".to_string()];
        let loaded = load(&request).expect("load");
        assert_eq!(
            ids(&loaded.modules),
            vec![
                "app:main".to_string(),
                "app:test".to_string(),
                "lib:main".to_string()
            ]
        );
    }

    #[test]
    fn a_module_glob_lists_child_modules() {
        let tree = Temp::new("glob");
        tree.write("project.yaml", "modules:\n  - libs/*\n");
        tree.write("libs/a/module.yaml", "product: jvm/lib\n");
        tree.write("libs/a/src/A.kt", "class A\n");
        tree.write("libs/b/module.yaml", "product: jvm/lib\n");
        tree.write("libs/b/src/B.kt", "class B\n");
        tree.write("libs/notes.txt", "not a module\n");

        let loaded = load(&command(&tree.0)).expect("load");
        assert_eq!(
            ids(&loaded.modules),
            vec!["libs/a:main".to_string(), "libs/b:main".to_string()]
        );
    }

    #[test]
    fn settings_and_non_jvm_products_are_rejected() {
        let settings = Temp::new("settings");
        settings.write(
            "module.yaml",
            "product: jvm/app\nsettings:\n  kotlin:\n    version: 2.2.21\n",
        );
        settings.write("src/main.kt", "fun main() {}\n");
        assert_eq!(
            load(&command(&settings.0)).unwrap_err(),
            format!(
                "{}: unsupported key 'settings'",
                settings.0.join("module.yaml").display()
            )
        );

        let native = Temp::new("native");
        native.write("module.yaml", "product: linux/app\n");
        native.write("src/main.kt", "fun main() {}\n");
        assert_eq!(
            load(&command(&native.0)).unwrap_err(),
            format!(
                "{}: unsupported product type 'linux/app'; krusty-toolchain build compiles jvm/app and jvm/lib",
                native.0.join("module.yaml").display()
            )
        );
    }

    #[test]
    fn an_unknown_module_names_the_modules_that_exist() {
        let tree = Temp::new("missing");
        tree.write("project.yaml", "modules:\n  - app\n  - lib\n");
        tree.write("app/module.yaml", "product: jvm/app\n");
        tree.write("app/src/main.kt", "fun main() {}\n");
        tree.write("lib/module.yaml", "product: jvm/lib\n");
        tree.write("lib/src/Lib.kt", "class Lib\n");
        let mut request = command(&tree.0);
        request.modules = vec!["missing".to_string()];
        assert_eq!(
            load(&request).unwrap_err(),
            "no module named 'missing'.\nAvailable modules:\n- app\n- lib"
        );
    }

    #[test]
    fn an_iml_project_stays_an_extension() {
        let idea = Temp::new("idea");
        idea.write(".idea/modules.xml", "<project/>");
        assert_eq!(
            load(&command(&idea.0)).unwrap_err(),
            format!(
                "krusty-toolchain build compiles Kotlin Toolchain projects (module.yaml or project.yaml).\n\
                 {} is a JetBrains .iml project. .iml support remains a project-model extension and is not compiled by this command yet.",
                idea.0.display()
            )
        );
    }

    #[test]
    fn a_toolchain_marker_outranks_gradle_and_the_closest_root_wins() {
        let tree = Temp::new("priority");
        tree.write("module.yaml", "product: jvm/app\n");
        tree.write("src/main.kt", "fun main() {}\n");
        tree.write("build.gradle.kts", "");
        assert!(load(&command(&tree.0)).is_ok());

        let nested = Temp::new("nested");
        nested.write("project.yaml", "modules:\n  - app\n");
        nested.write("app/module.yaml", "product: jvm/app\n");
        nested.write("app/src/main.kt", "fun main() {}\n");
        nested.write("app/sample/build.gradle.kts", "??? not groovy\n");
        let sample = nested.0.join("app/sample");
        let source = sample.join("custom-src");
        std::fs::create_dir_all(&source).expect("source");
        std::fs::write(source.join("Main.kt"), "fun main() {}\n").expect("source");
        let loaded = load_using(
            &command(&sample),
            &crate::kotlin_toolchain::tool::FnRunner(
                |invocation: &crate::kotlin_toolchain::tool::ToolCommand| {
                    assert_eq!(invocation.directory, sample);
                    assert!(invocation
                        .args
                        .iter()
                        .any(|arg| arg == "krustyToolchainModel"));
                    Ok(crate::kotlin_toolchain::tool::ToolOutput {
                        status: 0,
                        stdout: format!(
                            "KRUSTY\t:main\tsample\t{}\t0\nSRC\t:main\t{}\n",
                            sample.display(),
                            source.display()
                        ),
                        stderr: String::new(),
                    })
                },
            ),
        )
        .expect("nested gradle");
        assert_eq!(loaded.root, sample);
        assert_eq!(loaded.modules[0].source_roots[0].path, source);

        let inside = nested.0.join("app/src");
        let loaded = load(&command(&inside)).expect("parent toolchain");
        assert_eq!(loaded.root, nested.0);
    }

    #[test]
    fn unsupported_platforms_and_variants_are_rejected() {
        let tree = Temp::new("flags");
        tree.write("module.yaml", "product: jvm/app\n");
        tree.write("src/main.kt", "fun main() {}\n");
        let mut request = command(&tree.0);
        request.platforms = vec!["iosArm64".to_string()];
        assert_eq!(
            load(&request).unwrap_err(),
            "krusty-toolchain build compiles the JVM. Unsupported platform(s): iosArm64"
        );
        request.platforms.clear();
        request.variants = vec!["preview".to_string()];
        assert_eq!(
            load(&request).unwrap_err(),
            "invalid variant(s): preview. Supported values: debug, release"
        );
    }
}
