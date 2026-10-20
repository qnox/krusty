//! The plan of `compileJvm` tasks: the modules to compile, in order, each with its compiler
//! arguments, its resources and the modules it waits for, built completely, with every refusal,
//! before anything runs.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::arguments::{arguments, Compilation};
use super::classpath::{classpath, runtime_classpath};
use super::ksp::{self, Ksp, KspOutputs, KspRun};
use super::order::{compile_dependencies, compile_order};
use super::refusals::at_setting;
use super::{jdk, plugins, refusals, sources};
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
    /// Where the resources are copied. It holds exactly the resource directories' contents, and
    /// nothing when the fragment has none.
    pub resources_output: PathBuf,
    /// The fragment's resources directory, when it has one, then the directories of resources its
    /// plugins add; a file of a later directory replaces the same file of an earlier one.
    pub resources: Vec<PathBuf>,
    /// The KSP run that comes first, when the fragment runs KSP processors.
    pub ksp: Option<Ksp>,
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
        let directory = header.file.parent().unwrap_or(Path::new(""));
        let (generated, plugins_refused) =
            match plugins::generated(name, directory, configuration, &model.project.plugins) {
                Ok(generated) => (generated, Vec::new()),
                Err(refused) => (plugins::Generated::default(), refused),
            };
        let refused = refusals::refusals(name, configuration, main);
        let (processors, processors_refused) = ksp_processors(name, configuration, settings);
        let mut compiles =
            refused.is_empty() && plugins_refused.is_empty() && processors_refused.is_empty();
        for diagnostic in plugins_refused
            .into_iter()
            .chain(refused)
            .chain(processors_refused)
        {
            diagnostics.push(diagnostic);
        }
        let ksp_outputs = (!processors.is_empty()).then(|| KspOutputs::of(root, name));
        let jdk_home = jdk::select(
            name,
            configuration,
            settings,
            environment.java_home.as_deref(),
        )
        .map_err(|diagnostic| diagnostics.push(diagnostic))
        .ok();
        let mut layout = sources::layout(
            directory,
            &configuration
                .module
                .as_ref()
                .and_then(|whole| whole.text_at(&["layout"]))
                .unwrap_or_default(),
        );
        layout.sources.extend(generated.sources);
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
        for file in &found.not_directories {
            compiles = false;
            diagnostics.push(Diagnostic::error(
                file,
                None,
                format!(
                    "Source directory at '{}' exists, but it's not a directory, this is currently unsupported",
                    file.display()
                ),
            ));
        }
        let artifacts = artifact_directory(root, name);
        let output = artifacts.join("kotlin-output");
        let resources_output = artifacts.join("resources-output");
        // The fragment's resources directory, then the plugins' resource directories, each copied
        // over the ones before it as the toolchain does.
        let resources: Vec<PathBuf> = layout
            .resources
            .is_dir()
            .then(|| layout.resources.clone())
            .into_iter()
            .chain(generated.resources)
            .chain(
                ksp_outputs
                    .as_ref()
                    .map(|outputs| outputs.resources.clone()),
            )
            .collect();
        // Copying a directory that holds the resources output would copy the output into itself.
        if let Some(holding) = resources
            .iter()
            .find(|directory| resources_output.starts_with(directory))
        {
            compiles = false;
            diagnostics.push(Diagnostic::project_error(format!(
                "the resource directory `{}` of module `{name}` holds its resources output `{}`, and krusty-toolchain does not copy a directory into itself",
                holding.display(),
                resources_output.display()
            )));
        }
        outputs.insert(
            name,
            [
                (!found.kotlin.is_empty()).then(|| output.clone()),
                (!resources.is_empty()).then(|| resources_output.clone()),
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
        let ksp_classpaths = (!processors.is_empty()).then(|| {
            let version = settings
                .text_at(&["kotlin", "ksp", "version"])
                .unwrap_or_default();
            let (mut graph, resolver) =
                resolvers.resolve_artifacts(&declarations[index], &ksp::ksp_artifacts(&version));
            let tool = runtime_classpath(&mut graph, resolver, &store, &mut problems);
            let (mut graph, resolver) =
                resolvers.resolve_artifacts(&declarations[index], &processors);
            let processors = runtime_classpath(&mut graph, resolver, &store, &mut problems);
            (tool, processors)
        });
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
        let ksp = match ksp_classpaths {
            Some((tool, processors)) => {
                let existing: Vec<PathBuf> = layout
                    .sources
                    .iter()
                    .filter(|directory| directory.is_dir())
                    .cloned()
                    .collect();
                let jdk_version = settings
                    .text_at(&["jvm", "jdk", "version"])
                    .and_then(|version| version.parse().ok())
                    .unwrap_or_default();
                let ksp = ksp::ksp(&KspRun {
                    root,
                    module: name,
                    module_directory: directory,
                    jdk_home: &jdk_home,
                    jdk_version,
                    jvm_target: release.map_or(jdk_version, i64::from),
                    kotlin_version: &kotlin_version,
                    ksp_classpath: &tool,
                    processors: &processors,
                    options: &processor_options(settings),
                    sources: &existing,
                    libraries: &entries,
                })?;
                // The module compiles against what KSP generates, last.
                entries.push(ksp.outputs.classes.clone());
                Some(ksp)
            }
            None => None,
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
                fragment_sources: (found.filled > 1).then_some(found.walked.as_slice()),
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
            ksp,
            waits,
        });
    }
    Ok((!diagnostics.has_errors()).then_some(Plan { steps }))
}

/// The Maven artifacts `settings.kotlin.ksp.processors` of `module` names, with a refusal of each
/// processor that is a module or a catalog entry.
fn ksp_processors(
    module: &str,
    configuration: &crate::configuration::Configuration,
    settings: &crate::tree::Node,
) -> (Vec<Coordinates>, Vec<Diagnostic>) {
    let Some(Value::List(items)) = settings
        .at(&["kotlin", "ksp", "processors"])
        .map(|node| &node.value)
    else {
        return (Vec::new(), Vec::new());
    };
    let mut processors = Vec::new();
    let mut refused = Vec::new();
    for item in items {
        match resolution::maven_artifact(item) {
            Some(coordinates) => processors.push(coordinates),
            None => refused.push(at_setting(
                configuration,
                item,
                format!(
                    "module `{module}` runs a KSP processor that is not a Maven artifact, and krusty-toolchain runs only processors published to a Maven repository"
                ),
            )),
        }
    }
    (processors, refused)
}

/// `settings.kotlin.ksp.processorOptions`, in the order written.
fn processor_options(settings: &crate::tree::Node) -> Vec<(String, String)> {
    match settings
        .at(&["kotlin", "ksp", "processorOptions"])
        .map(|node| &node.value)
    {
        Some(Value::Mapping { entries, .. }) => entries
            .iter()
            .filter_map(|entry| match &entry.value.value {
                Value::String(value) => Some((entry.key.clone(), value.clone())),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}
