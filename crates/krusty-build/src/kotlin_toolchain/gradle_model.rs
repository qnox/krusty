//! Load a Gradle project by running Gradle.
//!
//! An init script asks Gradle for source sets, JVM Kotlin compilations, and Android variants.
//! Gradle evaluates the build scripts. The script text is never read here; only the line protocol
//! Gradle prints is.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::model::{Module, ModuleId, ModuleOutput, SourceRoot, SourceRootKind};

use super::load::{self, FoundSources, LoadedProject};
use super::tool::{self, ToolCommand, ToolRunner};

const TASK: &str = "krustyToolchainModel";

const INIT_SCRIPT: &str = r#"// Injected with --init-script. The project is not modified.
def emit = { String line -> System.out.println(line) }

def moduleId = { project, String name ->
    def path = project.path == ":" ? "" : project.path
    path + ":" + name
}

def emitPaths = { String id, String kind, paths ->
    if (paths == null) return
    paths.each { path ->
        if (path != null) emit(kind + "\t" + id + "\t" + path.toString())
    }
}

gradle.rootProject.tasks.register("krustyToolchainModel") {
    doLast {
        rootProject.allprojects { project ->
            def state = [android: false, kmp: false]
            try {
                def android = project.extensions.findByName("android")
                if (android != null) {
                    def boot = []
                    try { boot = android.bootClasspath.collect { it.absolutePath } } catch (ignored) {}
                    def variants = android.hasProperty("applicationVariants") ? android.applicationVariants :
                        (android.hasProperty("libraryVariants") ? android.libraryVariants : [])
                    variants.each { variant ->
                        state.android = true
                        def id = moduleId(project, variant.name)
                        def test = variant.name.toLowerCase().contains("test") ? "1" : "0"
                        emit("KRUSTY\t" + id + "\t" + project.name + "\t" + project.projectDir.absolutePath + "\t" + test)
                        def roots = [] as Set
                        variant.sourceSets.each { sourceSet ->
                            sourceSet.javaDirectories?.each { roots << it.absolutePath }
                            try { sourceSet.kotlinDirectories?.each { roots << it.absolutePath } } catch (ignored) {}
                        }
                        emitPaths(id, "SRC", roots.sort())
                        def classpath = []
                        classpath.addAll(boot)
                        try { classpath.addAll(variant.getCompileClasspath(null).files.collect { it.absolutePath }) } catch (ignored) {}
                        emitPaths(id, "CP", classpath.unique())
                        try {
                            emit("OUT\t" + id + "\t" + variant.javaCompileProvider.get().destinationDirectory.get().asFile.absolutePath)
                        } catch (ignored) {}
                        try {
                            def taskName = "compile${variant.name.capitalize()}Kotlin"
                            def compileTask = project.tasks.findByName(taskName)
                            (compileTask?.compilerOptions?.freeCompilerArgs?.getOrElse([]) ?: []).each {
                                emit("ARG\t" + id + "\t" + it.toString())
                            }
                        } catch (ignored) {}
                    }
                }
            } catch (ignored) {}
            if (state.android) return

            try {
                def kotlin = project.extensions.findByName("kotlin")
                if (kotlin != null && project.plugins.hasPlugin("org.jetbrains.kotlin.multiplatform")) {
                    kotlin.targets.findAll { target -> target.platformType?.name == "jvm" }.each { target ->
                        target.compilations.each { compilation ->
                            state.kmp = true
                            def id = moduleId(project, target.name + "/" + compilation.name)
                            def test = compilation.name.toLowerCase().contains("test") ? "1" : "0"
                            emit("KRUSTY\t" + id + "\t" + project.name + "\t" + project.projectDir.absolutePath + "\t" + test)
                            def roots = [] as Set
                            compilation.allKotlinSourceSets.each { sourceSet ->
                                sourceSet.kotlin?.srcDirs?.each { roots << it.absolutePath }
                            }
                            try {
                                def javaSourceSet = project.extensions.findByName("sourceSets")?.findByName(compilation.defaultSourceSet?.name)
                                javaSourceSet?.allJava?.srcDirs?.each { roots << it.absolutePath }
                            } catch (ignored) {}
                            emitPaths(id, "SRC", roots.sort())
                            try { emitPaths(id, "CP", compilation.compileDependencyFiles?.collect { it.absolutePath } ?: []) } catch (ignored) {}
                            try { emitPaths(id, "OUT", compilation.output?.classesDirs?.files?.collect { it.absolutePath } ?: []) } catch (ignored) {}
                            try {
                                def associates = compilation.associateWith.collect { it.output?.classesDirs?.files }.flatten().findAll { it != null }.collect { it.absolutePath }
                                emitPaths(id, "FRIEND", associates)
                            } catch (ignored) {}
                            try {
                                def jvm = compilation.compilerOptions?.options?.jvmTarget?.getOrNull()?.target
                                if (jvm != null) emit("JVM\t" + id + "\t" + jvm.toString())
                            } catch (ignored) {}
                            try {
                                (compilation.compileTaskProvider.get().compilerOptions?.freeCompilerArgs?.getOrElse([]) ?: []).each {
                                    emit("ARG\t" + id + "\t" + it.toString())
                                }
                            } catch (ignored) {}
                        }
                    }
                }
            } catch (ignored) {}
            if (state.kmp) return

            def sourceSets = project.extensions.findByName("sourceSets")
            if (sourceSets == null) return
            sourceSets.each { set ->
                def id = moduleId(project, set.name)
                def test = (set.name == "test" || set.name.endsWith("Test") || set.name.endsWith("test")) ? "1" : "0"
                emit("KRUSTY\t" + id + "\t" + project.name + "\t" + project.projectDir.absolutePath + "\t" + test)
                def roots = [] as Set
                set.allJava?.srcDirs?.each { roots << it.absolutePath }
                try { set.extensions.findByName("kotlin")?.srcDirs?.each { roots << it.absolutePath } } catch (ignored) {}
                emitPaths(id, "SRC", roots.sort())
                try {
                    def configuration = project.configurations.findByName(set.compileClasspathConfigurationName)
                    if (configuration != null) {
                        emitPaths(id, "CP", configuration.incoming.artifactView { view -> view.lenient = true }.files.collect { it.absolutePath })
                    }
                } catch (ignored) {}
                try { emitPaths(id, "OUT", set.output?.classesDirs?.files?.collect { it.absolutePath } ?: []) } catch (ignored) {}
                try {
                    def compileTask = project.tasks.findByName(set.getCompileTaskName("kotlin"))
                    def jvm = compileTask?.compilerOptions?.jvmTarget?.getOrNull()?.target
                    if (jvm != null) emit("JVM\t" + id + "\t" + jvm.toString())
                    (compileTask?.compilerOptions?.freeCompilerArgs?.getOrElse([]) ?: []).each {
                        emit("ARG\t" + id + "\t" + it.toString())
                    }
                } catch (ignored) {}
                try {
                    def configuration = project.configurations.findByName(set.compileClasspathConfigurationName)
                    (configuration?.allDependencies ?: []).findAll { it instanceof org.gradle.api.artifacts.ProjectDependency }.each { dependency ->
                        def path = dependency.path ?: dependency.dependencyProject.path
                        emit("DEP\t" + id + "\t" + ((path == ":") ? ":main" : path + ":main"))
                    }
                } catch (ignored) {}
                if (test == "1") {
                    emit("DEP\t" + id + "\t" + moduleId(project, "main"))
                    try {
                        sourceSets.findByName("main")?.output?.classesDirs?.files?.each {
                            emit("FRIEND\t" + id + "\t" + it.absolutePath)
                        }
                    } catch (ignored) {}
                }
                try {
                    set.resources?.srcDirs?.each { dir ->
                        def files = dir.isDirectory() ? dir.listFiles() : null
                        if (files != null && files.any { it.isFile() || it.isDirectory() }) {
                            emit("RES\t" + id + "\t" + dir.absolutePath)
                        }
                    }
                } catch (ignored) {}
            }
        }
    }
}
"#;

struct Reported {
    id: String,
    name: String,
    directory: PathBuf,
    test: bool,
    sources: Vec<PathBuf>,
    classpath: Vec<PathBuf>,
    outputs: Vec<PathBuf>,
    args: Vec<String>,
    jvm: Option<String>,
    dependencies: Vec<String>,
    friends: Vec<PathBuf>,
    resources: Vec<PathBuf>,
}

pub(super) fn load_project(
    root: &Path,
    names: &[String],
    runner: &dyn ToolRunner,
) -> Result<LoadedProject, String> {
    let program = tool::tool_program(root, "gradlew", "gradlew.bat", "gradle");
    let script = root.join("build/krusty/init.gradle");
    if let Some(parent) = script.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    std::fs::write(&script, INIT_SCRIPT)
        .map_err(|error| format!("cannot write {}: {error}", script.display()))?;
    let output = runner.run(&ToolCommand {
        program: program.clone(),
        args: vec![
            "--quiet".to_string(),
            "--console=plain".to_string(),
            "--no-configuration-cache".to_string(),
            "--init-script".to_string(),
            script.display().to_string(),
            TASK.to_string(),
        ],
        directory: root.to_path_buf(),
    })?;
    tool::require_success(&program, &output)?;
    let reported = parse_model(root, &output.stdout)?;
    let mut modules = modules_from_report(root, &reported)?;
    if modules.is_empty() {
        return Err(format!(
            "{} has no Kotlin sources to compile",
            root.display()
        ));
    }
    modules = load::select_reported(modules, names)?;
    Ok(LoadedProject {
        root: root.to_path_buf(),
        modules,
    })
}

fn parse_model(root: &Path, stdout: &str) -> Result<Vec<Reported>, String> {
    let mut modules: Vec<Reported> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    for line in stdout.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let Some((kind, rest)) = line.split_once('\t') else {
            continue;
        };
        match kind {
            "KRUSTY" => {
                let fields = split_fields(root, line, rest, 4)?;
                if index.contains_key(fields[0]) {
                    return Err(format!(
                        "{}: gradle reported module '{}' twice",
                        root.display(),
                        fields[0]
                    ));
                }
                let test = match fields[3] {
                    "1" => true,
                    "0" => false,
                    other => {
                        return Err(format!(
                            "{}: gradle module '{}' has test flag '{other}'",
                            root.display(),
                            fields[0]
                        ));
                    }
                };
                require_absolute(root, fields[0], "directory", fields[2])?;
                index.insert(fields[0].to_string(), modules.len());
                modules.push(Reported {
                    id: fields[0].to_string(),
                    name: fields[1].to_string(),
                    directory: PathBuf::from(fields[2]),
                    test,
                    sources: Vec::new(),
                    classpath: Vec::new(),
                    outputs: Vec::new(),
                    args: Vec::new(),
                    jvm: None,
                    dependencies: Vec::new(),
                    friends: Vec::new(),
                    resources: Vec::new(),
                });
            }
            "SRC" | "CP" | "OUT" | "FRIEND" | "RES" | "DEP" | "JVM" | "ARG" => {
                let module = current_field(root, &mut modules, &index, line, rest)?;
                match kind {
                    "SRC" => {
                        require_absolute(root, &module.id, "source root", &rest_path(line, rest)?)?;
                        push_unique(&mut module.sources, PathBuf::from(rest_path(line, rest)?));
                    }
                    "CP" => {
                        require_absolute(
                            root,
                            &module.id,
                            "classpath entry",
                            &rest_path(line, rest)?,
                        )?;
                        push_unique(&mut module.classpath, PathBuf::from(rest_path(line, rest)?));
                    }
                    "OUT" => {
                        require_absolute(root, &module.id, "output", &rest_path(line, rest)?)?;
                        push_unique(&mut module.outputs, PathBuf::from(rest_path(line, rest)?));
                    }
                    "FRIEND" => {
                        require_absolute(root, &module.id, "friend path", &rest_path(line, rest)?)?;
                        push_unique(&mut module.friends, PathBuf::from(rest_path(line, rest)?));
                    }
                    "RES" => {
                        require_absolute(
                            root,
                            &module.id,
                            "resource directory",
                            &rest_path(line, rest)?,
                        )?;
                        push_unique(&mut module.resources, PathBuf::from(rest_path(line, rest)?));
                    }
                    "DEP" => {
                        let dep = rest_path(line, rest)?;
                        if !module.dependencies.iter().any(|existing| existing == &dep) {
                            module.dependencies.push(dep);
                        }
                    }
                    "JVM" => {
                        let target = rest_path(line, rest)?;
                        if module.jvm.is_none() {
                            module.jvm = Some(target);
                        }
                    }
                    "ARG" => {
                        let argument = rest_path(line, rest)?;
                        if !module.args.iter().any(|existing| existing == &argument) {
                            module.args.push(argument);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if modules.is_empty() {
        return Err(format!("{}: gradle reported no modules", root.display()));
    }
    Ok(modules)
}

fn current_field<'a>(
    root: &Path,
    modules: &'a mut [Reported],
    index: &BTreeMap<String, usize>,
    line: &str,
    rest: &str,
) -> Result<&'a mut Reported, String> {
    let id = rest.split('\t').next().unwrap_or("");
    if id.is_empty() {
        return Err(format!(
            "{}: gradle model line is incomplete: {line}",
            root.display()
        ));
    }
    let Some(slot) = index.get(id).copied() else {
        return Err(format!(
            "{}: gradle model line names unknown module '{id}': {line}",
            root.display()
        ));
    };
    Ok(&mut modules[slot])
}

fn rest_path(line: &str, rest: &str) -> Result<String, String> {
    let Some((_, path)) = rest.split_once('\t') else {
        return Err(format!("gradle model line is incomplete: {line}"));
    };
    if path.is_empty() || path.contains('\t') {
        return Err(format!("gradle model line is incomplete: {line}"));
    }
    Ok(path.to_string())
}

fn split_fields<'a>(
    root: &Path,
    line: &str,
    rest: &'a str,
    count: usize,
) -> Result<Vec<&'a str>, String> {
    let fields: Vec<&str> = rest.split('\t').collect();
    if fields.len() != count || fields.iter().any(|field| field.is_empty()) {
        return Err(format!(
            "{}: gradle model line is incomplete: {line}",
            root.display()
        ));
    }
    Ok(fields)
}

fn require_absolute(root: &Path, id: &str, what: &str, path: &str) -> Result<(), String> {
    if Path::new(path).is_absolute() {
        Ok(())
    } else {
        Err(format!(
            "{}: gradle reported a relative {what} for '{id}': {path}",
            root.display()
        ))
    }
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn modules_from_report(root: &Path, reported: &[Reported]) -> Result<Vec<Module>, String> {
    let mut scanned: Vec<Option<FoundSources>> = Vec::with_capacity(reported.len());
    for module in reported {
        let found = load::scan_compilation_roots(root, &module.sources)?;
        if found.has_kotlin || !found.java.is_empty() {
            scanned.push(Some(found));
        } else {
            scanned.push(None);
        }
    }
    let produced: BTreeMap<&str, usize> = reported
        .iter()
        .enumerate()
        .filter(|(index, _)| scanned[*index].is_some())
        .map(|(index, module)| (module.id.as_str(), index))
        .collect();
    let mut modules = Vec::new();
    for (index, module) in reported.iter().enumerate() {
        let Some(found) = scanned[index].as_ref() else {
            continue;
        };
        let mut built = Module::new(ModuleId::new(&module.id), &module.directory);
        built.display_name = module.name.clone();
        built.module_name = Some(module.name.clone());
        let kind = if module.test {
            SourceRootKind::Test
        } else {
            SourceRootKind::Main
        };
        built.source_roots = found
            .roots
            .iter()
            .map(|path| SourceRoot {
                path: path.clone(),
                kind,
                generated: false,
            })
            .collect();
        built.java_sources = found.java.clone();
        built.resources = module.resources.clone();
        built.classpath = module.classpath.clone();
        built.jvm_target = module.jvm.clone();
        built.kotlinc_args = module.args.clone();
        built.outputs = vec![ModuleOutput::ClassDirectory(choose_output(
            root,
            &module.id,
            module.test,
            &module.outputs,
        ))];
        built.friend_paths = module.friends.clone();
        for dependency in &module.dependencies {
            if produced.contains_key(dependency.as_str()) {
                built.depends_on.push(ModuleId::new(dependency));
            } else if reported.iter().all(|candidate| candidate.id != *dependency) {
                return Err(format!(
                    "{}: module '{}' depends on '{dependency}', which Gradle did not report",
                    root.display(),
                    module.id
                ));
            }
        }
        modules.push(built);
    }
    Ok(modules)
}

fn choose_output(root: &Path, id: &str, test: bool, outputs: &[PathBuf]) -> PathBuf {
    if outputs.len() == 1
        && outputs[0]
            .extension()
            .is_none_or(|extension| extension != "jar")
    {
        outputs[0].clone()
    } else {
        let mut safe = String::new();
        for ch in id.chars() {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                safe.push(ch);
            } else {
                safe.push('_');
            }
        }
        if safe.is_empty() {
            safe.push_str("module");
        }
        root.join("build/krusty/modules").join(safe).join(if test {
            "test-classes"
        } else {
            "classes"
        })
    }
}
