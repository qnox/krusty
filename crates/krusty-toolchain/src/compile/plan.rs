//! The plan of `compileJvm` tasks: the modules to compile, in order, each with its compiler
//! arguments, its resources and the modules it waits for, built completely, with every refusal,
//! before anything runs.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::arguments::{arguments, Compilation};
use super::classpath::classpath;
use super::order::{compile_dependencies, compile_order};
use super::{jdk, refusals, sources};
use crate::diagnostic::{Diagnostic, Diagnostics};
use crate::maven::{missing, Coordinates, Metadata, OpenError, Problem, Store};
use crate::model::Model;
use crate::resolution;
use crate::tree::Value;

/// What a compilation takes from the host rather than the project.
pub struct Environment {
    /// `JAVA_HOME`, the only JDK krusty-toolchain compiles against.
    pub java_home: Option<PathBuf>,
}

/// One module's main fragment.
pub struct Step {
    pub module: String,
    /// Where the compiled classes go (`-d`). It holds exactly this run's classes, and nothing
    /// when the fragment has no Kotlin sources.
    pub output: PathBuf,
    /// krusty's arguments; `None` when the fragment has no Kotlin sources, which the toolchain
    /// does not compile.
    pub arguments: Option<Vec<OsString>>,
    /// Where the last successful compilation's inputs and output are recorded (`avoidance.rs`).
    pub state: PathBuf,
    /// Where the resources are copied. It holds exactly the resources directory's contents, and
    /// nothing when the fragment has no resources directory.
    pub resources_output: PathBuf,
    /// The fragment's resources directory, when it has one.
    pub resources: Option<PathBuf>,
    /// The earlier steps this one starts after: those of the modules the fragment compiles against
    /// directly, which start after the ones they compile against.
    pub waits: Vec<usize>,
}

/// The modules to compile, each after the modules it compiles against.
pub struct Plan {
    pub steps: Vec<Step>,
}

/// The coordinates of the Kotlin serialization compiler plugin of a Kotlin release.
const SERIALIZATION_PLUGIN: (&str, &str) = (
    "org.jetbrains.kotlin",
    "kotlin-serialization-compiler-plugin-embeddable",
);

/// Where the toolchain puts what it builds for a module's JVM main fragment.
fn artifact_directory(root: &Path, module: &str) -> PathBuf {
    root.join("build")
        .join("artifacts")
        .join("CompiledJvmArtifact")
        .join(format!("{module}jvm"))
}

/// Where krusty-toolchain records a module's last successful compilation, outside the toolchain's
/// own artifacts.
fn state_file(root: &Path, module: &str) -> PathBuf {
    root.join("build")
        .join("krusty-toolchain")
        .join(format!("{module}jvm"))
        .join("compilation.state")
}

/// The plan for compiling the modules `modules` of `model`, or `None` when something it needs is
/// refused: why goes to `diagnostics`, for every module the plan needs, in the order they would
/// compile.
pub fn plan(
    model: &Model,
    modules: &[&str],
    environment: &Environment,
    diagnostics: &mut Diagnostics,
) -> Result<Option<Plan>, String> {
    let declarations = resolution::read_declarations(&model.modules, &model.configured)?;
    let targets = modules
        .iter()
        .map(|module| {
            model
                .modules
                .iter()
                .position(|header| header.name == *module)
                .ok_or_else(|| format!("no module is named `{module}`"))
        })
        .collect::<Result<Vec<usize>, String>>()?;
    let dependencies = compile_dependencies(&declarations);
    let order = match compile_order(&dependencies, &targets) {
        Ok(order) => order,
        Err(cycle) => {
            let names: Vec<&str> = cycle
                .iter()
                .map(|&index| model.modules[index].name.as_str())
                .collect();
            diagnostics.push(Diagnostic::project_error(format!(
                "modules depend on each other for compilation in a cycle ({}), and krusty-toolchain cannot order their compilation",
                names.join(" -> ")
            )));
            return Ok(None);
        }
    };
    let maven_local = declarations.iter().any(|module| module.maven_local);
    let store = match Store::for_project(maven_local) {
        Ok(store) => store,
        Err(OpenError::NoCacheRoot(message)) => return Err(message.to_string()),
        Err(OpenError::LocalRepository(problem)) => {
            diagnostics.push(problem.diagnostic());
            return Ok(None);
        }
    };
    let metadata = Metadata::new(&store);
    let mut resolvers = resolution::Resolvers::new(&metadata);
    let root = &model.project.root;
    let mut problems: Vec<Problem> = Vec::new();
    let mut steps = Vec::new();
    // The outputs of the modules planned so far, which their dependents compile against: the
    // compiled output when the module has Kotlin sources, then the resources output when it has
    // resources.
    let mut outputs: HashMap<&str, Vec<PathBuf>> = HashMap::new();
    // The step of each module planned so far, by module index.
    let mut planned_steps: HashMap<usize, usize> = HashMap::new();
    for index in order {
        let header = &model.modules[index];
        let name = header.name.as_str();
        let configuration = &model.configured[index];
        let Some(main) = &configuration.main else {
            return Err(format!("module `{name}` is not configured"));
        };
        let settings = main.get("settings").ok_or("a fragment without settings")?;
        let refused = refusals::refusals(name, configuration, main);
        let compiles = refused.is_empty();
        for diagnostic in refused {
            diagnostics.push(diagnostic);
        }
        let jdk_home = jdk::select(
            name,
            configuration,
            settings,
            environment.java_home.as_deref(),
        )
        .map_err(|diagnostic| diagnostics.push(diagnostic))
        .ok();
        let directory = header.file.parent().unwrap_or(Path::new(""));
        let layout = sources::layout(
            directory,
            &configuration
                .module
                .as_ref()
                .and_then(|whole| whole.text_at(&["layout"]))
                .unwrap_or_default(),
        );
        let found = match sources::find(&layout.sources) {
            Ok(found) => found,
            Err(error) => {
                diagnostics.push(Diagnostic::error(
                    directory,
                    None,
                    format!("cannot read the sources of module `{name}`: {error}"),
                ));
                // Reported, so there is no plan; its dependents still report their own refusals.
                outputs.insert(name, Vec::new());
                continue;
            }
        };
        let artifacts = artifact_directory(root, name);
        let output = artifacts.join("kotlin-output");
        let resources_output = artifacts.join("resources-output");
        let resources = layout.resources.is_dir().then(|| layout.resources.clone());
        outputs.insert(
            name,
            [
                (!found.kotlin.is_empty()).then(|| output.clone()),
                resources.as_ref().map(|_| resources_output.clone()),
            ]
            .into_iter()
            .flatten()
            .collect(),
        );
        if let Some(java) = found.java.first() {
            diagnostics.push(Diagnostic::error(
                java,
                None,
                format!(
                    "module `{name}` has Java sources, and krusty-toolchain compiles Kotlin sources only"
                ),
            ));
        }
        let known = problems.len();
        let (mut graphs, resolver) = resolvers.resolve_module(&declarations, index, false);
        let resolved = classpath(&mut graphs[0], resolver, &store, &mut problems);
        let kotlin_version = settings.text_at(&["kotlin", "version"]).unwrap_or_default();
        let mut plugins = Vec::new();
        if settings.is_enabled(&["kotlin", "serialization", "enabled"]) {
            let (group, artifact) = SERIALIZATION_PLUGIN;
            let coordinates = Coordinates::new(group, artifact, Some(&kotlin_version));
            let file = format!("{}.jar", coordinates.file_stem());
            match store.artifact_file(&coordinates, &file) {
                Ok(Some(path)) => plugins.push(path),
                Ok(None) => problems.push(missing(&coordinates, &format!("file `{file}`"))),
                Err(problem) => problems.push(problem),
            }
        }
        let new_problems = problems.len() > known;
        for problem in &problems[known..] {
            diagnostics.push(problem.diagnostic());
        }
        let Some(jdk_home) = jdk_home else { continue };
        if !compiles || !found.java.is_empty() || new_problems {
            continue;
        }
        let mut entries: Vec<PathBuf> = Vec::new();
        for module in &resolved.modules {
            let planned = outputs.get(module.as_str()).ok_or_else(|| {
                format!("module `{module}` is compiled against before it is planned")
            })?;
            entries.extend(planned.iter().cloned());
        }
        entries.extend(resolved.files);
        let release = match settings.at(&["jvm", "release"]).map(|node| &node.value) {
            Some(Value::Int(release)) => Some(*release),
            _ => None,
        };
        let arguments = if found.kotlin.is_empty() {
            None
        } else {
            Some(arguments(&Compilation {
                release,
                jdk_home: &jdk_home,
                classpath: &entries,
                module_name: name,
                java_parameters: settings.is_enabled(&["jvm", "storeParameterNames"]),
                plugins: &plugins,
                kotlin_version: &kotlin_version,
                output: &output,
                sources: &found.kotlin,
            })?)
        };
        let waits: Option<Vec<usize>> = dependencies[index]
            .iter()
            .map(|dependency| planned_steps.get(dependency).copied())
            .collect();
        let Some(waits) = waits else {
            // A module it compiles against has no step because it was refused, so there is no
            // plan; anything else is a mistake in planning.
            if diagnostics.has_errors() {
                continue;
            }
            return Err(format!(
                "module `{name}` compiles against a module that is not planned before it"
            ));
        };
        planned_steps.insert(index, steps.len());
        steps.push(Step {
            module: name.to_string(),
            output,
            arguments,
            state: state_file(root, name),
            resources_output,
            resources,
            waits,
        });
    }
    Ok((!diagnostics.has_errors()).then_some(Plan { steps }))
}
