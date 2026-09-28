//! Gradle and Maven are asked for the project. Their descriptors are not read.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::{load_using, BuildCommand};
use crate::graph::ModuleGraph;
use crate::kotlin_toolchain::tool::{FnRunner, ToolCommand, ToolOutput};
use crate::model::Module;

struct Temp(PathBuf);

impl Temp {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "krusty-toolchain-tools-{label}-{}-{}",
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

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find_map(|pair| (pair[0] == name).then_some(pair[1].as_str()))
}

fn expression(args: &[String]) -> Option<&str> {
    args.iter()
        .find_map(|arg| arg.strip_prefix("-Dexpression="))
}

fn output_file(args: &[String]) -> Option<&str> {
    args.iter()
        .find_map(|arg| arg.strip_prefix("-Dmdep.outputFile="))
}

#[test]
fn a_gradle_build_is_read_by_running_gradle() {
    let tree = Temp::new("gradle-run");
    tree.write("build.gradle.kts", "??? not groovy\n");
    tree.write("settings.gradle.kts", "??? not groovy\n");
    let lib_src = tree.0.join("lib-custom");
    let app_src = tree.0.join("app-custom");
    let app_test = tree.0.join("app-test-custom");
    std::fs::create_dir_all(&lib_src).expect("lib");
    std::fs::create_dir_all(&app_src).expect("app");
    std::fs::create_dir_all(&app_test).expect("test");
    std::fs::write(lib_src.join("Lib.kt"), "class Lib\n").expect("lib");
    std::fs::write(lib_src.join("Lib.java"), "class LibJava {}\n").expect("java");
    std::fs::write(app_src.join("Main.kt"), "fun main() {}\n").expect("app");
    std::fs::write(app_test.join("MainTest.kt"), "fun check() {}\n").expect("test");
    let lib_out = tree.0.join("lib-classes");
    let model = format!(
        "noise that is not a model line\n\
         KRUSTY\t:lib:main\tlib\t{root}\t0\n\
         SRC\t:lib:main\t{lib_src}\n\
         CP\t:lib:main\t/repo/lib.jar\n\
         OUT\t:lib:main\t{lib_out}\n\
         JVM\t:lib:main\t17\n\
         KRUSTY\t:app:main\tapp\t{root}\t0\n\
         SRC\t:app:main\t{app_src}\n\
         DEP\t:app:main\t:lib:main\n\
         OUT\t:app:main\t/out/one\n\
         OUT\t:app:main\t/out/two\n\
         ARG\t:app:main\t-Xexplicit-api=strict\n\
         KRUSTY\t:app:test\tapp\t{root}\t1\n\
         SRC\t:app:test\t{app_test}\n\
         DEP\t:app:test\t:app:main\n\
         FRIEND\t:app:test\t/out/one\n",
        root = tree.0.display(),
        lib_src = lib_src.display(),
        app_src = app_src.display(),
        app_test = app_test.display(),
        lib_out = lib_out.display(),
    );
    let loaded = load_using(
        &command(&tree.0),
        &FnRunner(|invocation: &ToolCommand| {
            assert_eq!(invocation.program, PathBuf::from("gradle"));
            assert_eq!(invocation.directory, tree.0);
            assert!(invocation.args.iter().any(|arg| arg == "--init-script"));
            assert!(invocation
                .args
                .iter()
                .any(|arg| arg == "krustyToolchainModel"));
            assert!(invocation
                .args
                .iter()
                .any(|arg| arg == "--no-configuration-cache"));
            let script = PathBuf::from(flag(&invocation.args, "--init-script").expect("script"));
            let text = std::fs::read_to_string(&script).expect("init script");
            assert!(text.contains("sourceSets"));
            assert!(!text.contains("???"));
            Ok(ToolOutput {
                status: 0,
                stdout: model.clone(),
                stderr: String::new(),
            })
        }),
    )
    .expect("gradle");

    let lib = module(&loaded.modules, ":lib:main");
    assert_eq!(lib.source_roots[0].path, lib_src);
    assert_eq!(lib.java_sources, vec![lib_src.join("Lib.java")]);
    assert!(!lib.is_krusty_compilable());
    assert_eq!(lib.classpath, vec![PathBuf::from("/repo/lib.jar")]);
    assert_eq!(lib.outputs[0].path(), lib_out.as_path());
    assert_eq!(lib.jvm_target.as_deref(), Some("17"));

    let app = module(&loaded.modules, ":app:main");
    assert_eq!(app.source_roots[0].path, app_src);
    assert_eq!(deps(app), vec![":lib:main".to_string()]);
    assert_eq!(app.kotlinc_args, vec!["-Xexplicit-api=strict".to_string()]);
    assert_eq!(app.outputs.len(), 1);
    assert!(app.outputs[0]
        .path()
        .ends_with("build/krusty/modules/_app_main/classes"));

    let test = module(&loaded.modules, ":app:test");
    assert_eq!(deps(test), vec![":app:main".to_string()]);
    assert_eq!(test.friend_paths, vec![PathBuf::from("/out/one")]);

    let mut graph = ModuleGraph::new();
    for module in &loaded.modules {
        graph.insert(module.clone()).expect("insert");
    }
    graph.validate().expect("edges resolve");
}

#[test]
fn a_failed_gradle_run_reports_gradles_own_diagnostic() {
    let tree = Temp::new("gradle-fail");
    tree.write("build.gradle.kts", "??? not groovy\n");
    let error = load_using(
        &command(&tree.0),
        &FnRunner(|_invocation: &ToolCommand| {
            Ok(ToolOutput {
                status: 1,
                stdout: String::new(),
                stderr: "Script compilation error: unexpected token\n".to_string(),
            })
        }),
    )
    .unwrap_err();
    assert_eq!(
        error,
        "gradle exited with status 1: Script compilation error: unexpected token"
    );
}

#[test]
fn a_maven_build_is_read_by_running_maven() {
    let tree = Temp::new("maven-run");
    tree.write("pom.xml", "this is not a pom\n");
    let source = tree.0.join("custom-kotlin");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::write(source.join("Main.kt"), "fun main() {}\n").expect("source");
    let output = tree.0.join("custom-classes");
    let loaded = load_using(
        &command(&tree.0),
        &FnRunner(|invocation: &ToolCommand| {
            assert_eq!(invocation.program, PathBuf::from("mvn"));
            assert!(invocation.args.iter().any(|arg| arg == "-f"));
            assert_eq!(
                flag(&invocation.args, "-f").map(PathBuf::from),
                Some(tree.0.join("pom.xml"))
            );
            if invocation
                .args
                .iter()
                .any(|arg| arg == "dependency:build-classpath")
            {
                let file = output_file(&invocation.args).expect("classpath file");
                std::fs::write(file, "/repo/app.jar\n").expect("classpath");
                return Ok(ToolOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }
            let value = match expression(&invocation.args).expect("expression") {
                "project.modules" => "[]",
                "project.packaging" => "jar",
                "project.artifactId" => "app",
                "project.basedir" => tree.0.to_str().expect("utf8"),
                "project.compileSourceRoots" => return Ok(list_output(&source)),
                "project.testCompileSourceRoots" => "[]",
                "project.build.outputDirectory" => output.to_str().expect("utf8"),
                "project.build.testOutputDirectory" => "null",
                other => panic!("unexpected expression {other}"),
            };
            Ok(ToolOutput {
                status: 0,
                stdout: format!("{value}\n"),
                stderr: String::new(),
            })
        }),
    )
    .expect("maven");
    let app = module(&loaded.modules, "app:main");
    assert_eq!(app.source_roots[0].path, source);
    assert_eq!(app.classpath, vec![PathBuf::from("/repo/app.jar")]);
    assert_eq!(app.outputs[0].path(), output.as_path());
    assert!(loaded.modules.iter().all(|module| {
        module
            .id
            .as_ref()
            .is_some_and(|id| id.as_str() != "app:test")
    }));
}

#[test]
fn a_maven_reactor_module_is_selected_with_pl() {
    let tree = Temp::new("maven-reactor");
    tree.write("pom.xml", "this is not a pom\n");
    let source = tree.0.join("lib-kotlin");
    let lib_base = tree.0.join("lib");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::write(source.join("Lib.kt"), "class Lib\n").expect("source");
    let loaded = load_using(
        &command(&tree.0),
        &FnRunner(|invocation: &ToolCommand| {
            let selected = flag(&invocation.args, "-pl");
            if invocation
                .args
                .iter()
                .any(|arg| arg == "dependency:build-classpath")
            {
                assert_eq!(selected, Some("lib"));
                let file = output_file(&invocation.args).expect("classpath file");
                std::fs::write(file, "/repo/lib.jar\n").expect("classpath");
                return Ok(empty_output());
            }
            let value = match (expression(&invocation.args).expect("expression"), selected) {
                ("project.modules", None) => "[lib]",
                ("project.packaging", None) => "pom",
                ("project.modules", Some("lib")) => "[]",
                ("project.packaging", Some("lib")) => "jar",
                ("project.artifactId", Some("lib")) => "lib",
                ("project.basedir", Some("lib")) => lib_base.to_str().expect("utf8"),
                ("project.compileSourceRoots", Some("lib")) => {
                    return Ok(list_output(&source));
                }
                ("project.testCompileSourceRoots", Some("lib")) => "[]",
                ("project.build.outputDirectory", Some("lib")) => "null",
                ("project.build.testOutputDirectory", Some("lib")) => "null",
                (expression, selected) => {
                    panic!("unexpected {expression} for {selected:?}")
                }
            };
            Ok(ToolOutput {
                status: 0,
                stdout: format!("{value}\n"),
                stderr: String::new(),
            })
        }),
    )
    .expect("reactor");
    let lib = module(&loaded.modules, "lib:main");
    assert_eq!(lib.source_roots[0].path, source);
    assert_eq!(lib.classpath, vec![PathBuf::from("/repo/lib.jar")]);
    assert!(lib.outputs[0]
        .path()
        .ends_with("build/krusty/modules/lib/classes"));
}

#[test]
fn gradle_wins_over_maven_and_a_toolchain_file_wins_over_both() {
    let both = Temp::new("both-tools");
    both.write("build.gradle.kts", "??? not groovy\n");
    both.write("pom.xml", "this is not a pom\n");
    let saw_gradle = AtomicUsize::new(0);
    let _ = load_using(
        &command(&both.0),
        &FnRunner(|invocation: &ToolCommand| {
            assert_eq!(invocation.program, PathBuf::from("gradle"));
            saw_gradle.fetch_add(1, Ordering::Relaxed);
            Ok(ToolOutput {
                status: 1,
                stdout: String::new(),
                stderr: "probe\n".to_string(),
            })
        }),
    )
    .unwrap_err();
    assert_eq!(saw_gradle.load(Ordering::Relaxed), 1);

    let toolchain = Temp::new("toolchain-wins");
    toolchain.write("module.yaml", "product: jvm/app\n");
    toolchain.write("src/main.kt", "fun main() {}\n");
    toolchain.write("pom.xml", "this is not a pom\n");
    toolchain.write("build.gradle.kts", "??? not groovy\n");
    load_using(
        &command(&toolchain.0),
        &FnRunner(|_invocation: &ToolCommand| {
            Err("a toolchain project must not run Gradle or Maven".to_string())
        }),
    )
    .expect("toolchain");
}

#[test]
fn a_nested_maven_directory_wins_over_a_parent_toolchain_project() {
    let tree = Temp::new("nested-maven");
    tree.write("project.yaml", "modules:\n  - app\n");
    tree.write("app/module.yaml", "product: jvm/app\n");
    tree.write("app/src/main.kt", "fun main() {}\n");
    tree.write("app/sample/pom.xml", "this is not a pom\n");
    let sample = tree.0.join("app/sample");
    let source = sample.join("kotlin-src");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::write(source.join("Main.kt"), "fun main() {}\n").expect("source");
    let loaded = load_using(
        &command(&sample),
        &FnRunner(|invocation: &ToolCommand| {
            assert_eq!(invocation.directory, sample);
            assert_eq!(invocation.program, PathBuf::from("mvn"));
            if invocation
                .args
                .iter()
                .any(|arg| arg == "dependency:build-classpath")
            {
                let file = output_file(&invocation.args).expect("classpath");
                std::fs::write(file, "").expect("empty classpath");
                return Ok(empty_output());
            }
            let value = match expression(&invocation.args).expect("expression") {
                "project.modules" => "[]",
                "project.packaging" => "jar",
                "project.artifactId" => "sample",
                "project.basedir" => sample.to_str().expect("utf8"),
                "project.compileSourceRoots" => return Ok(list_output(&source)),
                "project.testCompileSourceRoots" => "[]",
                "project.build.outputDirectory" => "null",
                "project.build.testOutputDirectory" => "null",
                other => panic!("unexpected expression {other}"),
            };
            Ok(ToolOutput {
                status: 0,
                stdout: format!("{value}\n"),
                stderr: String::new(),
            })
        }),
    )
    .expect("nested maven");
    assert_eq!(loaded.root, sample);
    assert_eq!(
        module(&loaded.modules, "sample:main").source_roots[0].path,
        source
    );
}

#[test]
fn maven_coordinates_are_resolved_by_maven_and_exported_to_dependents() {
    let tree = Temp::new("coords");
    tree.write("project.yaml", "modules:\n  - app\n  - lib\n  - edge\n");
    tree.write(
        "lib/module.yaml",
        "product: jvm/lib\ndependencies:\n  - org.apache.commons:commons-lang3:3.14.0: exported\n  - io.ktor:ktor-client-java:2.3.0: runtime-only\n",
    );
    tree.write("lib/src/Lib.kt", "class Lib\n");
    tree.write(
        "app/module.yaml",
        "product: jvm/app\ndependencies:\n  - //lib\n  - com.google.guava:guava:32.1.0: compile-only\n",
    );
    tree.write("app/src/main.kt", "fun main() {}\n");
    tree.write(
        "edge/module.yaml",
        "product: jvm/lib\ndependencies:\n  - //app\n",
    );
    tree.write("edge/src/Edge.kt", "class Edge\n");
    let calls = AtomicUsize::new(0);
    let loaded = load_using(
        &command(&tree.0),
        &FnRunner(|invocation: &ToolCommand| {
            calls.fetch_add(1, Ordering::Relaxed);
            assert!(invocation
                .args
                .iter()
                .any(|arg| arg == "dependency:build-classpath"));
            let pom = std::fs::read_to_string(flag(&invocation.args, "-f").expect("pom"))
                .expect("pom text");
            assert!(!pom.contains("runtime-only"));
            let file = output_file(&invocation.args).expect("classpath");
            let jar = if pom.contains("commons-lang3") {
                "/repo/commons.jar"
            } else if pom.contains("guava") {
                "/repo/guava.jar"
            } else {
                panic!("unexpected coordinate pom:\n{pom}");
            };
            std::fs::write(file, format!("{jar}\n")).expect("classpath");
            Ok(empty_output())
        }),
    )
    .expect("coordinates");
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "runtime-only is not resolved, and a coordinate is resolved once"
    );
    assert_eq!(
        classpath(module(&loaded.modules, "lib:main")),
        vec!["/repo/commons.jar".to_string()]
    );
    assert_eq!(
        classpath(module(&loaded.modules, "app:main")),
        vec![
            "/repo/guava.jar".to_string(),
            "/repo/commons.jar".to_string()
        ]
    );
    assert_eq!(
        classpath(module(&loaded.modules, "edge:main")),
        Vec::<String>::new()
    );

    let hidden = Temp::new("hidden-export");
    hidden.write("project.yaml", "modules:\n  - app\n  - lib\n");
    hidden.write(
        "lib/module.yaml",
        "product: jvm/lib\ndependencies:\n  - org.apache.commons:commons-lang3:3.14.0\n",
    );
    hidden.write("lib/src/Lib.kt", "class Lib\n");
    hidden.write(
        "app/module.yaml",
        "product: jvm/app\ndependencies:\n  - //lib\n",
    );
    hidden.write("app/src/main.kt", "fun main() {}\n");
    let loaded = load_using(
        &command(&hidden.0),
        &FnRunner(|invocation: &ToolCommand| {
            let file = output_file(&invocation.args).expect("classpath");
            std::fs::write(file, "/repo/commons.jar\n").expect("classpath");
            Ok(empty_output())
        }),
    )
    .expect("hidden");
    assert_eq!(
        classpath(module(&loaded.modules, "lib:main")),
        vec!["/repo/commons.jar".to_string()]
    );
    assert!(classpath(module(&loaded.modules, "app:main")).is_empty());
}

#[test]
fn a_failed_maven_resolution_reports_mavens_own_diagnostic() {
    let tree = Temp::new("maven-fail");
    tree.write(
        "module.yaml",
        "product: jvm/app\ndependencies:\n  - io.ktor:ktor-client-java:2.3.0\n",
    );
    tree.write("src/main.kt", "fun main() {}\n");
    let error = load_using(
        &command(&tree.0),
        &FnRunner(|_invocation: &ToolCommand| {
            Ok(ToolOutput {
                status: 1,
                stdout: String::new(),
                stderr: "Could not find artifact io.ktor:ktor-client-java:jar:2.3.0\n".to_string(),
            })
        }),
    )
    .unwrap_err();
    assert_eq!(
        error,
        "mvn exited with status 1: Could not find artifact io.ktor:ktor-client-java:jar:2.3.0"
    );
}

#[test]
fn the_maven_wrapper_is_preferred_when_it_is_present() {
    let tree = Temp::new("mvnw");
    tree.write("mvnw", "");
    tree.write(
        "module.yaml",
        "product: jvm/app\ndependencies:\n  - io.ktor:ktor-client-java:2.3.0\n",
    );
    tree.write("src/main.kt", "fun main() {}\n");
    let error = load_using(
        &command(&tree.0),
        &FnRunner(|invocation: &ToolCommand| {
            assert_eq!(invocation.program, tree.0.join("mvnw"));
            Ok(ToolOutput {
                status: 1,
                stdout: String::new(),
                stderr: "wrapper\n".to_string(),
            })
        }),
    )
    .unwrap_err();
    assert_eq!(
        error,
        format!(
            "{} exited with status 1: wrapper",
            tree.0.join("mvnw").display()
        )
    );
}

fn module<'a>(modules: &'a [Module], id: &str) -> &'a Module {
    modules
        .iter()
        .find(|module| {
            module
                .id
                .as_ref()
                .is_some_and(|module_id| module_id.as_str() == id)
        })
        .unwrap_or_else(|| panic!("missing module {id}"))
}

fn deps(module: &Module) -> Vec<String> {
    module
        .depends_on
        .iter()
        .map(|id| id.as_str().to_string())
        .collect()
}

fn classpath(module: &Module) -> Vec<String> {
    module
        .classpath
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

fn list_output(path: &Path) -> ToolOutput {
    ToolOutput {
        status: 0,
        stdout: format!("[{}]\n", path.display()),
        stderr: String::new(),
    }
}

fn empty_output() -> ToolOutput {
    ToolOutput {
        status: 0,
        stdout: String::new(),
        stderr: String::new(),
    }
}
