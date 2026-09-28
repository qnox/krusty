//! Load Maven projects and Maven coordinates by running Maven.
//!
//! `help:evaluate` and `dependency:build-classpath` print the model and the classpath. POM XML,
//! parent POMs, and Gradle module metadata are not read. A coordinate from `module.yaml` is handed
//! to Maven through a POM this crate writes; the library's own descriptors stay inside Maven.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::model::{Module, ModuleId, ModuleOutput, SourceRoot, SourceRootKind};

use super::load::{self, LoadedProject};
use super::tool::{self, ToolCommand, ToolOutput, ToolRunner};

pub(super) struct MavenResolver<'a> {
    runner: &'a dyn ToolRunner,
    root: PathBuf,
    program: PathBuf,
    cache: HashMap<String, Vec<PathBuf>>,
}

impl<'a> MavenResolver<'a> {
    pub(super) fn new(runner: &'a dyn ToolRunner, root: &Path) -> Self {
        Self {
            runner,
            root: root.to_path_buf(),
            program: tool::tool_program(root, "mvnw", "mvnw.cmd", "mvn"),
            cache: HashMap::new(),
        }
    }

    pub(super) fn jars(&mut self, coordinate: &str) -> Result<Vec<PathBuf>, String> {
        if let Some(cached) = self.cache.get(coordinate) {
            return Ok(cached.clone());
        }
        let jars = resolve_coordinate(self.runner, &self.program, &self.root, coordinate)?;
        self.cache.insert(coordinate.to_string(), jars.clone());
        Ok(jars)
    }
}

pub(super) fn is_maven_coordinate(notation: &str) -> bool {
    let parts: Vec<&str> = notation.split(':').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && !part.chars().any(char::is_whitespace))
}

pub(super) fn load_project(
    root: &Path,
    names: &[String],
    runner: &dyn ToolRunner,
) -> Result<LoadedProject, String> {
    let program = tool::tool_program(root, "mvnw", "mvnw.cmd", "mvn");
    let pom = root.join("pom.xml");
    let mut modules = Vec::new();
    let mut visited = BTreeSet::new();
    walk(
        root,
        &program,
        runner,
        &pom,
        None,
        &mut visited,
        &mut modules,
    )?;
    if modules.is_empty() {
        return Err(format!(
            "{} has no Kotlin sources to compile",
            root.display()
        ));
    }
    Ok(LoadedProject {
        root: root.to_path_buf(),
        modules: load::select_reported(modules, names)?,
    })
}

fn walk(
    root: &Path,
    program: &Path,
    runner: &dyn ToolRunner,
    pom: &Path,
    pl: Option<&str>,
    visited: &mut BTreeSet<String>,
    modules: &mut Vec<Module>,
) -> Result<(), String> {
    let key = format!("{}@{}", pom.display(), pl.unwrap_or(""));
    if !visited.insert(key) {
        return Err(format!(
            "{}: maven reported a module cycle at {}",
            root.display(),
            pl.unwrap_or(".")
        ));
    }
    let packaging = evaluate_scalar(root, program, runner, pom, pl, "project.packaging")?;
    let children = evaluate_list(root, program, runner, pom, pl, "project.modules")?;
    if packaging != "pom" {
        describe(root, program, runner, pom, pl, modules)?;
    }
    for child in children {
        let child_pl = match pl {
            Some(parent) => format!("{parent}/{child}"),
            None => child,
        };
        walk(
            root,
            program,
            runner,
            pom,
            Some(&child_pl),
            visited,
            modules,
        )?;
    }
    Ok(())
}

struct Described {
    artifact: String,
    base: PathBuf,
    main_roots: Vec<PathBuf>,
    test_roots: Vec<PathBuf>,
    main_out: PathBuf,
    test_out: PathBuf,
}

fn describe(
    root: &Path,
    program: &Path,
    runner: &dyn ToolRunner,
    pom: &Path,
    pl: Option<&str>,
    modules: &mut Vec<Module>,
) -> Result<(), String> {
    let artifact = evaluate_scalar(root, program, runner, pom, pl, "project.artifactId")?;
    if modules.iter().any(|module| module.display_name == artifact) {
        return Err(format!(
            "module name '{artifact}' is used by more than one module"
        ));
    }
    let base = PathBuf::from(evaluate_scalar(
        root,
        program,
        runner,
        pom,
        pl,
        "project.basedir",
    )?);
    if !base.is_absolute() {
        return Err(format!(
            "{}: maven reported a relative basedir for '{artifact}': {}",
            root.display(),
            base.display()
        ));
    }
    let main_roots = rooted(
        &base,
        &evaluate_list(root, program, runner, pom, pl, "project.compileSourceRoots")?,
    );
    let test_roots = rooted(
        &base,
        &evaluate_list(
            root,
            program,
            runner,
            pom,
            pl,
            "project.testCompileSourceRoots",
        )?,
    );
    let described = Described {
        main_out: output_directory(
            root,
            &artifact,
            false,
            evaluate_optional(
                root,
                program,
                runner,
                pom,
                pl,
                "project.build.outputDirectory",
            )?,
        ),
        test_out: output_directory(
            root,
            &artifact,
            true,
            evaluate_optional(
                root,
                program,
                runner,
                pom,
                pl,
                "project.build.testOutputDirectory",
            )?,
        ),
        artifact,
        base,
        main_roots,
        test_roots,
    };
    let main = load::scan_compilation_roots(root, &described.main_roots)?;
    let test = load::scan_compilation_roots(root, &described.test_roots)?;
    let main_compiles = main.has_kotlin || !main.java.is_empty();
    let test_compiles = test.has_kotlin || !test.java.is_empty();
    let main_cp = if main_compiles {
        classpath(
            root,
            program,
            runner,
            pom,
            pl,
            "compile",
            &described.artifact,
        )?
    } else {
        Vec::new()
    };
    let test_cp = if test_compiles {
        classpath(root, program, runner, pom, pl, "test", &described.artifact)?
    } else {
        Vec::new()
    };
    if main_compiles {
        modules.push(maven_module(
            &described,
            false,
            &main,
            &described.main_out,
            main_cp,
            Vec::new(),
        ));
    }
    if test_compiles {
        let mut depends_on = Vec::new();
        let mut friends = Vec::new();
        if main_compiles {
            depends_on.push(ModuleId::new(format!("{}:main", described.artifact)));
            friends.push(described.main_out.clone());
        }
        let mut module = maven_module(
            &described,
            true,
            &test,
            &described.test_out,
            test_cp,
            depends_on,
        );
        module.friend_paths = friends;
        modules.push(module);
    }
    Ok(())
}

fn maven_module(
    described: &Described,
    test: bool,
    found: &load::FoundSources,
    output: &Path,
    classpath: Vec<PathBuf>,
    depends_on: Vec<ModuleId>,
) -> Module {
    let suffix = if test { "test" } else { "main" };
    let mut module = Module::new(
        ModuleId::new(format!("{}:{suffix}", described.artifact)),
        &described.base,
    );
    module.display_name = described.artifact.clone();
    module.module_name = Some(described.artifact.clone());
    let kind = if test {
        SourceRootKind::Test
    } else {
        SourceRootKind::Main
    };
    module.source_roots = found
        .roots
        .iter()
        .map(|path| SourceRoot {
            path: path.clone(),
            kind,
            generated: false,
        })
        .collect();
    module.java_sources = found.java.clone();
    module.classpath = classpath;
    module.outputs = vec![ModuleOutput::ClassDirectory(output.to_path_buf())];
    module.depends_on = depends_on;
    module
}

fn rooted(base: &Path, paths: &[String]) -> Vec<PathBuf> {
    paths
        .iter()
        .map(|path| {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                base.join(path)
            }
        })
        .collect()
}

fn output_directory(root: &Path, artifact: &str, test: bool, reported: Option<String>) -> PathBuf {
    if let Some(path) = reported {
        let path = PathBuf::from(path);
        if path.is_absolute() && path.extension().is_none_or(|extension| extension != "jar") {
            return path;
        }
    }
    let leaf = if test { "test-classes" } else { "classes" };
    root.join("build/krusty/modules").join(artifact).join(leaf)
}

fn classpath(
    root: &Path,
    program: &Path,
    runner: &dyn ToolRunner,
    pom: &Path,
    pl: Option<&str>,
    scope: &str,
    artifact: &str,
) -> Result<Vec<PathBuf>, String> {
    let file = root
        .join("build/krusty/maven")
        .join(safe_name(artifact))
        .join(format!("{scope}-classpath.txt"));
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let output = run(
        root,
        program,
        runner,
        pom,
        pl,
        [
            "dependency:build-classpath".to_string(),
            format!("-Dmdep.outputFile={}", file.display()),
            format!("-Dmdep.includeScope={scope}"),
        ],
    )?;
    tool::require_success(program, &output)?;
    let text = std::fs::read_to_string(&file).map_err(|_| {
        format!(
            "{} wrote no classpath file at {}",
            program.display(),
            file.display()
        )
    })?;
    Ok(split_classpath(&text))
}

fn resolve_coordinate(
    runner: &dyn ToolRunner,
    program: &Path,
    root: &Path,
    coordinate: &str,
) -> Result<Vec<PathBuf>, String> {
    let (group, artifact, version) = split_coordinate(coordinate)?;
    let directory = root.join("build/krusty/maven").join(safe_name(coordinate));
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
    let pom = directory.join("pom.xml");
    let file = directory.join("classpath.txt");
    std::fs::write(&pom, resolve_pom(group, artifact, version))
        .map_err(|error| format!("cannot write {}: {error}", pom.display()))?;
    let output = runner.run(&ToolCommand {
        program: program.to_path_buf(),
        args: vec![
            "-B".to_string(),
            "-q".to_string(),
            "-f".to_string(),
            pom.display().to_string(),
            "dependency:build-classpath".to_string(),
            format!("-Dmdep.outputFile={}", file.display()),
            "-Dmdep.includeScope=compile".to_string(),
        ],
        directory: root.to_path_buf(),
    })?;
    tool::require_success(program, &output)?;
    let text = std::fs::read_to_string(&file).map_err(|_| {
        format!(
            "{} wrote no classpath file at {}",
            program.display(),
            file.display()
        )
    })?;
    Ok(split_classpath(&text))
}

fn split_coordinate(coordinate: &str) -> Result<(&str, &str, &str), String> {
    let mut parts = coordinate.split(':');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(group), Some(artifact), Some(version), None)
            if !group.is_empty() && !artifact.is_empty() && !version.is_empty() =>
        {
            Ok((group, artifact, version))
        }
        _ => Err(format!(
            "dependency '{coordinate}' is not a Maven coordinate"
        )),
    }
}

fn resolve_pom(group: &str, artifact: &str, version: &str) -> String {
    format!(
        "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<project>
  <modelVersion>4.0.0</modelVersion>
  <groupId>krusty.toolchain</groupId>
  <artifactId>resolve</artifactId>
  <version>0</version>
  <repositories>
    <repository>
      <id>central</id>
      <url>https://repo.maven.apache.org/maven2</url>
    </repository>
    <repository>
      <id>google</id>
      <url>https://dl.google.com/dl/android/maven2</url>
    </repository>
  </repositories>
  <dependencies>
    <dependency>
      <groupId>{}</groupId>
      <artifactId>{}</artifactId>
      <version>{}</version>
    </dependency>
  </dependencies>
</project>
",
        xml_escape(group),
        xml_escape(artifact),
        xml_escape(version)
    )
}

fn xml_escape(text: &str) -> String {
    let mut escaped = String::new();
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn safe_name(text: &str) -> String {
    let mut safe = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '.' {
            safe.push(ch);
        } else {
            safe.push('_');
        }
    }
    if safe.is_empty() {
        safe.push_str("coordinate");
    }
    safe
}

fn split_classpath(text: &str) -> Vec<PathBuf> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let separator = if cfg!(windows) { ';' } else { ':' };
    text.split(separator)
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn evaluate_scalar(
    root: &Path,
    program: &Path,
    runner: &dyn ToolRunner,
    pom: &Path,
    pl: Option<&str>,
    expression: &str,
) -> Result<String, String> {
    let value = evaluate_raw(root, program, runner, pom, pl, expression)?;
    if value == "null" {
        Err(format!(
            "{} evaluated {expression} as null",
            program.display()
        ))
    } else {
        Ok(value)
    }
}

fn evaluate_optional(
    root: &Path,
    program: &Path,
    runner: &dyn ToolRunner,
    pom: &Path,
    pl: Option<&str>,
    expression: &str,
) -> Result<Option<String>, String> {
    let value = evaluate_raw(root, program, runner, pom, pl, expression)?;
    if value == "null" {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

fn evaluate_list(
    root: &Path,
    program: &Path,
    runner: &dyn ToolRunner,
    pom: &Path,
    pl: Option<&str>,
    expression: &str,
) -> Result<Vec<String>, String> {
    let value = evaluate_raw(root, program, runner, pom, pl, expression)?;
    parse_maven_list(&value).map_err(|()| {
        format!(
            "{} evaluated {expression} as '{value}', which is not a list",
            program.display()
        )
    })
}

fn evaluate_raw(
    root: &Path,
    program: &Path,
    runner: &dyn ToolRunner,
    pom: &Path,
    pl: Option<&str>,
    expression: &str,
) -> Result<String, String> {
    let output = run(
        root,
        program,
        runner,
        pom,
        pl,
        [
            "help:evaluate".to_string(),
            format!("-Dexpression={expression}"),
            "-DforceStdout".to_string(),
        ],
    )?;
    tool::require_success(program, &output)?;
    tool::last_line(&output.stdout)
        .ok_or_else(|| format!("{} returned no value for {expression}", program.display()))
}

fn run(
    root: &Path,
    program: &Path,
    runner: &dyn ToolRunner,
    pom: &Path,
    pl: Option<&str>,
    tail: [String; 3],
) -> Result<ToolOutput, String> {
    let mut args = vec![
        "-B".to_string(),
        "-q".to_string(),
        "-f".to_string(),
        pom.display().to_string(),
    ];
    if let Some(pl) = pl {
        args.push("-pl".into());
        args.push(pl.to_string());
    }
    args.extend(tail);
    runner.run(&ToolCommand {
        program: program.to_path_buf(),
        args,
        directory: root.to_path_buf(),
    })
}

/// Maven prints a Java list as `[a, b]`, `[]`, or `null`.
fn parse_maven_list(text: &str) -> Result<Vec<String>, ()> {
    if text == "null" {
        return Ok(Vec::new());
    }
    let Some(inner) = text
        .strip_prefix('[')
        .and_then(|text| text.strip_suffix(']'))
    else {
        return Err(());
    };
    if inner.is_empty() {
        return Ok(Vec::new());
    }
    Ok(inner.split(", ").map(str::to_string).collect())
}
