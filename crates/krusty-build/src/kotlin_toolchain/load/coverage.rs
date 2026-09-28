//! Fast checks for every `module.yaml` / `project.yaml` form the loader accepts or rejects.
//!
//! These tests call [`load`] only. They do not spawn the compiler.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{execute, load, BuildCommand};
use crate::graph::ModuleGraph;
use crate::model::{Module, SourceRootKind};

struct Temp(PathBuf);

impl Temp {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "krusty-toolchain-coverage-{label}-{}-{}",
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

fn module<'a>(modules: &'a [Module], id: &str) -> &'a Module {
    modules
        .iter()
        .find(|module| module.id.as_ref().unwrap().as_str() == id)
        .unwrap_or_else(|| panic!("missing module {id}"))
}

fn file_error(tree: &Temp, relative: &str, message: &str) -> String {
    format!("{}: {message}", tree.0.join(relative).display())
}

fn expect_module_error(contents: &str, message: &str) {
    let tree = Temp::new("schema");
    tree.write("module.yaml", contents);
    tree.write("src/main.kt", "fun main() {}\n");
    assert_eq!(
        load(&command(&tree.0)).unwrap_err(),
        file_error(&tree, "module.yaml", message),
        "module file:\n{contents}"
    );
}

#[test]
fn a_current_module_file_keeps_comments_quotes_and_the_long_product_form() {
    let tree = Temp::new("current");
    let name = tree.0.file_name().unwrap().to_string_lossy().into_owned();
    tree.write(
        "module.yaml",
        "\
# console application
product: \"jvm/app\"
layout: amper
description: |
  a module
dependencies:
",
    );
    tree.write("src/main.kt", "fun main() {}\n");
    tree.write("src/notes.md", "ignore\n");
    tree.write("src/com/example/Extra.kt", "class Extra\n");
    tree.write("testResources/fixture.txt", "fixture\n");

    let loaded = load(&command(&tree.0)).expect("load");
    assert_eq!(ids(&loaded.modules), vec![format!("{name}:main")]);
    let main = &loaded.modules[0];
    assert_eq!(main.module_name.as_deref(), Some(name.as_str()));
    assert_eq!(main.source_roots.len(), 1);
    assert_eq!(main.source_roots[0].path, tree.0.join("src"));
    assert_eq!(main.source_roots[0].kind, SourceRootKind::Main);
    assert!(main.resources.is_empty());
    assert!(main.java_sources.is_empty());
    assert_eq!(
        main.outputs[0].path(),
        tree.0
            .join("build/krusty/modules")
            .join(&name)
            .join("classes")
    );
}

#[test]
fn a_root_module_is_implicit_and_relative_dependencies_stay_inside_the_project() {
    let tree = Temp::new("root-module");
    let root_name = tree.0.file_name().unwrap().to_string_lossy().into_owned();
    tree.write(
        "module.yaml",
        "product: jvm/lib\ndependencies:\n  - ./lib\n",
    );
    tree.write("src/Root.kt", "class Root\n");
    tree.write("project.yaml", "modules:\n  - ./app\n  - ./lib\n");
    tree.write(
        "app/module.yaml",
        "product:\n  type: jvm/app\ndependencies:\n  - //\n  - ../lib\n",
    );
    tree.write("app/src/main.kt", "fun main() {}\n");
    tree.write("app/test/AppTest.kt", "fun check() {}\n");
    tree.write("lib/module.yaml", "product: jvm/lib\n");
    tree.write("lib/src/Lib.kt", "class Lib\n");
    tree.write("skipped/module.yaml", "product: jvm/lib\n");
    tree.write("skipped/src/Skipped.kt", "class Skipped\n");

    let loaded = load(&command(&tree.0)).expect("load");
    assert_eq!(loaded.root, tree.0);
    assert_eq!(
        ids(&loaded.modules),
        vec![
            format!("{root_name}:main"),
            "app:main".to_string(),
            "app:test".to_string(),
            "lib:main".to_string(),
        ]
    );
    assert_eq!(
        deps(module(&loaded.modules, &format!("{root_name}:main"))),
        vec!["lib:main".to_string()]
    );
    assert_eq!(
        deps(module(&loaded.modules, "app:main")),
        vec![format!("{root_name}:main"), "lib:main".to_string()]
    );
}

#[test]
fn a_module_filter_keeps_compile_only_exports_and_omits_runtime_only() {
    let tree = Temp::new("scopes");
    tree.write(
        "project.yaml",
        "modules:\n  - app\n  - lib\n  - core\n  - runtime\n",
    );
    tree.write(
        "app/module.yaml",
        "\
product: jvm/app
dependencies:
  - //lib: compile-only
test-dependencies:
  - //runtime:
      scope: runtime-only
",
    );
    tree.write("app/src/main.kt", "fun main() {}\n");
    tree.write("app/test/AppTest.kt", "fun check() {}\n");
    tree.write(
        "lib/module.yaml",
        "product: jvm/lib\ndependencies:\n  - //core: exported\n",
    );
    tree.write("lib/src/Lib.kt", "class Lib\n");
    tree.write("core/module.yaml", "product: jvm/lib\n");
    tree.write("core/src/Core.kt", "class Core\n");
    tree.write("runtime/module.yaml", "product: jvm/lib\n");
    tree.write("runtime/src/Runtime.kt", "class Runtime\n");

    let mut request = command(&tree.0);
    request.modules = vec!["app".to_string()];
    request.platforms = vec!["jvm".to_string()];
    request.variants = vec!["debug".to_string(), "release".to_string()];
    let loaded = load(&request).expect("load");
    assert_eq!(
        ids(&loaded.modules),
        vec![
            "app:main".to_string(),
            "app:test".to_string(),
            "lib:main".to_string(),
            "core:main".to_string(),
        ]
    );
    assert_eq!(
        deps(module(&loaded.modules, "app:main")),
        vec!["lib:main".to_string(), "core:main".to_string()]
    );
    assert_eq!(
        deps(module(&loaded.modules, "lib:main")),
        vec!["core:main".to_string()]
    );
    let test = module(&loaded.modules, "app:test");
    assert_eq!(test.source_roots[0].kind, SourceRootKind::Test);
    assert_eq!(
        deps(test),
        vec![
            "app:main".to_string(),
            "lib:main".to_string(),
            "core:main".to_string()
        ]
    );
}

#[test]
fn short_dependency_forms_are_not_reexported() {
    let tree = Temp::new("short-dep");
    tree.write(
        "project.yaml",
        "modules:\n  - user\n  - app\n  - lib\n  - extra\n  - core\n",
    );
    tree.write(
        "user/module.yaml",
        "product: jvm/app\ndependencies:\n  - //app\n",
    );
    tree.write("user/src/main.kt", "fun main() {}\n");
    tree.write(
        "app/module.yaml",
        "\
product: jvm/app
dependencies:
  - //lib:
  - //lib: all
  - //extra:
      scope: all
  - //core:
      exported: false
      scope: compile-only
",
    );
    tree.write("app/src/main.kt", "fun main() {}\n");
    tree.write("lib/module.yaml", "product: jvm/lib\n");
    tree.write("lib/src/Lib.kt", "class Lib\n");
    tree.write("extra/module.yaml", "product: jvm/lib\n");
    tree.write("extra/src/Extra.kt", "class Extra\n");
    tree.write("core/module.yaml", "product: jvm/lib\n");
    tree.write("core/src/Core.kt", "class Core\n");

    let loaded = load(&command(&tree.0)).expect("load");
    assert_eq!(
        deps(module(&loaded.modules, "user:main")),
        vec!["app:main".to_string()]
    );
    assert_eq!(
        deps(module(&loaded.modules, "app:main")),
        vec![
            "lib:main".to_string(),
            "extra:main".to_string(),
            "core:main".to_string()
        ]
    );
}

#[test]
fn test_sources_without_main_sources_have_no_friend_path() {
    let tree = Temp::new("test-only");
    let name = tree.0.file_name().unwrap().to_string_lossy().into_owned();
    tree.write("module.yaml", "product: jvm/lib\n");
    tree.write("test/LibTest.kt", "class LibTest\n");
    tree.write("testResources/fixture.txt", "fixture\n");

    let loaded = load(&command(&tree.0)).expect("load");
    assert_eq!(loaded.modules.len(), 1);
    let test = &loaded.modules[0];
    assert_eq!(test.id.as_ref().unwrap().as_str(), format!("{name}:test"));
    assert!(test.friend_paths.is_empty());
    assert!(test.depends_on.is_empty());
    assert_eq!(test.resources, vec![tree.0.join("testResources")]);
    assert_eq!(test.source_roots[0].path, tree.0.join("test"));
    assert_eq!(
        test.outputs[0].path(),
        tree.0
            .join("build/krusty/modules")
            .join(&name)
            .join("test-classes")
    );
}

#[test]
fn java_sources_are_recorded_and_a_script_is_rejected() {
    let java = Temp::new("java-only");
    java.write("module.yaml", "product: jvm/lib\nlayout: maven-like\n");
    java.write("src/main/java/Legacy.java", "class Legacy {}\n");
    java.write("src/main/resources/app.properties", "k=v\n");
    let loaded = load(&command(&java.0)).expect("load");
    assert_eq!(loaded.modules.len(), 1);
    assert!(!loaded.modules[0].is_krusty_compilable());
    assert_eq!(
        loaded.modules[0].java_sources,
        vec![java.0.join("src/main/java/Legacy.java")]
    );
    assert_eq!(
        loaded.modules[0].resources,
        vec![java.0.join("src/main/resources")]
    );
    assert_eq!(
        loaded.modules[0].source_roots[0].path,
        java.0.join("src/main/java")
    );

    let mixed = Temp::new("mixed");
    mixed.write("module.yaml", "product: jvm/lib\n");
    mixed.write("src/Lib.kt", "class Lib\n");
    mixed.write("src/Legacy.java", "class Legacy {}\n");
    let loaded = load(&command(&mixed.0)).expect("load");
    assert_eq!(loaded.modules.len(), 1);
    assert!(!loaded.modules[0].is_krusty_compilable());
    assert_eq!(
        loaded.modules[0].java_sources,
        vec![mixed.0.join("src/Legacy.java")]
    );

    let script = Temp::new("script");
    script.write("module.yaml", "product: jvm/app\n");
    script.write("src/com/script.kts", "println(1)\n");
    assert_eq!(
        load(&command(&script.0)).unwrap_err(),
        format!(
            "{}: Kotlin scripts are not compiled ({})",
            script.0.join("module.yaml").display(),
            script.0.join("src/com/script.kts").display()
        )
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_source_is_not_compiled() {
    let tree = Temp::new("symlink");
    let name = tree.0.file_name().unwrap().to_string_lossy().into_owned();
    tree.write("module.yaml", "product: jvm/app\n");
    tree.write("outside.kt", "fun main() {}\n");
    std::fs::create_dir_all(tree.0.join("src")).expect("src");
    std::os::unix::fs::symlink(tree.0.join("outside.kt"), tree.0.join("src/main.kt"))
        .expect("symlink");
    assert_eq!(
        load(&command(&tree.0)).unwrap_err(),
        file_error(
            &tree,
            "module.yaml",
            &format!("module '{name}' has no Kotlin sources to compile")
        )
    );
}

#[test]
fn project_lists_globs_and_unknown_schema_keys_fail_closed() {
    let duplicate = Temp::new("duplicate");
    duplicate.write("project.yaml", "modules:\n  - app\n  - app\n");
    duplicate.write("app/module.yaml", "product: jvm/app\n");
    duplicate.write("app/src/main.kt", "fun main() {}\n");
    assert_eq!(
        load(&command(&duplicate.0)).unwrap_err(),
        "project.yaml lists 'app' more than once"
    );

    let missing = Temp::new("missing-file");
    missing.write("project.yaml", "modules:\n  - app\n");
    assert_eq!(
        load(&command(&missing.0)).unwrap_err(),
        format!("missing {}", missing.0.join("app/module.yaml").display())
    );

    let stars = Temp::new("stars");
    stars.write("project.yaml", "modules:\n  - libs/*/*\n");
    assert_eq!(
        load(&command(&stars.0)).unwrap_err(),
        file_error(
            &stars,
            "project.yaml",
            "module pattern 'libs/*/*' is not a single-segment glob"
        )
    );

    let partial = Temp::new("partial-glob");
    partial.write("project.yaml", "modules:\n  - lib*\n");
    assert_eq!(
        load(&command(&partial.0)).unwrap_err(),
        file_error(
            &partial,
            "project.yaml",
            "module pattern 'lib*' is not a single-segment glob"
        )
    );

    let empty_glob = Temp::new("empty-glob");
    empty_glob.write("project.yaml", "modules:\n  - libs/*\n");
    empty_glob.write("libs/notes.txt", "not a module\n");
    assert_eq!(
        load(&command(&empty_glob.0)).unwrap_err(),
        file_error(
            &empty_glob,
            "project.yaml",
            "module pattern 'libs/*' matched no modules"
        )
    );

    let plugins = Temp::new("plugins");
    plugins.write("project.yaml", "modules:\n  - app\nplugins:\n  - //app\n");
    plugins.write("app/module.yaml", "product: jvm/app\n");
    plugins.write("app/src/main.kt", "fun main() {}\n");
    assert_eq!(
        load(&command(&plugins.0)).unwrap_err(),
        file_error(&plugins, "project.yaml", "unsupported key 'plugins'")
    );

    let not_list = Temp::new("modules-scalar");
    not_list.write("project.yaml", "modules: app\n");
    assert_eq!(
        load(&command(&not_list.0)).unwrap_err(),
        file_error(&not_list, "project.yaml", "modules must be a list")
    );

    let not_string = Temp::new("modules-map");
    not_string.write("project.yaml", "modules:\n  - name: app\n");
    assert_eq!(
        load(&command(&not_string.0)).unwrap_err(),
        file_error(&not_string, "project.yaml", "modules must be a string")
    );
}

#[test]
fn a_middle_glob_and_a_dot_entry_list_the_modules_they_name() {
    let tree = Temp::new("middle-glob");
    let root_name = tree.0.file_name().unwrap().to_string_lossy().into_owned();
    tree.write("project.yaml", "modules:\n  - .\n  - //libs/*/extra/\n");
    tree.write("module.yaml", "product: jvm/lib\n");
    tree.write("src/Root.kt", "class Root\n");
    tree.write("libs/a/extra/module.yaml", "product: jvm/lib\n");
    tree.write("libs/a/extra/src/A.kt", "class A\n");
    tree.write("libs/b/extra/module.yaml", "product: jvm/lib\n");
    tree.write("libs/b/extra/src/B.kt", "class B\n");
    tree.write("libs/a/other/module.yaml", "product: jvm/lib\n");
    tree.write("libs/a/other/src/Other.kt", "class Other\n");

    let loaded = load(&command(&tree.0)).expect("load");
    assert_eq!(
        ids(&loaded.modules),
        vec![
            format!("{root_name}:main"),
            "libs/a/extra:main".to_string(),
            "libs/b/extra:main".to_string(),
        ]
    );
}

#[test]
fn the_outermost_project_file_owns_nested_module_files() {
    let tree = Temp::new("outer");
    tree.write("project.yaml", "modules:\n  - app\n");
    tree.write("app/module.yaml", "product: jvm/app\n");
    tree.write("app/src/main.kt", "fun main() {}\n");
    tree.write("app/nested/project.yaml", "modules:\n  - hidden\n");
    tree.write("app/nested/hidden/module.yaml", "product: jvm/lib\n");
    tree.write("app/nested/hidden/src/Hidden.kt", "class Hidden\n");

    let start = tree.0.join("app/nested/hidden");
    let loaded = load(&command(&start)).expect("outer project");
    assert_eq!(loaded.root, tree.0);
    assert_eq!(ids(&loaded.modules), vec!["app:main".to_string()]);
}

#[test]
fn a_root_module_cannot_reuse_a_child_module_name() {
    let tree = Temp::new("collision");
    let root = tree.0.join("app");
    std::fs::create_dir_all(&root).expect("root");
    let project = Temp(root);
    project.write("module.yaml", "product: jvm/lib\n");
    project.write("src/Root.kt", "class Root\n");
    project.write("project.yaml", "modules:\n  - app\n");
    project.write("app/module.yaml", "product: jvm/lib\n");
    project.write("app/src/Lib.kt", "class Lib\n");
    assert_eq!(
        load(&command(&project.0)).unwrap_err(),
        "module name 'app' is used by more than one module"
    );
}

#[test]
fn module_schema_outside_jvm_products_and_project_dependencies_is_rejected() {
    let cases = [
        (
            "product: lib\n",
            "unsupported product type 'lib'; krusty-toolchain build compiles jvm/app and jvm/lib",
        ),
        (
            "product: kmp/lib\n",
            "unsupported product type 'kmp/lib'; krusty-toolchain build compiles jvm/app and jvm/lib",
        ),
        ("product:\n  - jvm/app\n", "product must be a type"),
        (
            "product:\n  platforms: [jvm]\n",
            "line 2: flow collections, anchors, and aliases are not supported",
        ),
        (
            "product:\n  platforms: jvm\n",
            "unsupported product key 'platforms'",
        ),
        (
            "product:\n  type:\n    - jvm/app\n",
            "product.type must be a string",
        ),
        ("description: hello\nlayout: amper\n", "missing product"),
        ("layout: gradle\nproduct: jvm/app\n", "unsupported layout 'gradle'"),
        (
            "product: jvm/app\nlayout:\n  - amper\n",
            "layout must be a string",
        ),
        (
            "product: jvm/app\napply:\n  - ./template.yaml\n",
            "unsupported key 'apply'",
        ),
        (
            "product: jvm/app\nrepositories:\n  - url: https://example.test\n",
            "unsupported key 'repositories'",
        ),
        (
            "product: jvm/app\nplugins:\n  demo:\n    enabled: true\n",
            "unsupported key 'plugins'",
        ),
        (
            "product: jvm/app\ndependencies@jvm:\n  - //lib\n",
            "unsupported key 'dependencies@jvm'",
        ),
        (
            "product: jvm/app\ndependencies:\n  - $libs.ktor\n",
            "dependency '$libs.ktor' uses a version catalog; krusty-toolchain build does not resolve catalogs yet",
        ),
        (
            "product: jvm/app\ndependencies:\n  - bom:imports\n",
            "dependency 'bom:imports' is not a project module",
        ),
        (
            "product: jvm/app\ndependencies:\n  - swiftPackage:org.example\n",
            "dependency 'swiftPackage:org.example' is not a project module",
        ),
        (
            "product: jvm/app\ndependencies:\n  - localSwiftPackage:demo\n",
            "dependency 'localSwiftPackage:demo' is not a project module",
        ),
        (
            "product: jvm/app\ndependencies: app\n",
            "dependencies must be a list",
        ),
        (
            "product: jvm/app\ntest-dependencies: lib\n",
            "dependencies must be a list",
        ),
        (
            "product: jvm/app\ndependencies:\n  - //lib:\n      exported: yes\n",
            "exported must be true or false, found 'yes'",
        ),
        (
            "product: jvm/app\ndependencies:\n  - //lib:\n      exported:\n        - true\n",
            "exported must be a string",
        ),
        (
            "product: jvm/app\ndependencies:\n  - //lib: mystery\n",
            "unsupported dependency attribute 'mystery'",
        ),
        (
            "product: jvm/app\ndependencies:\n  - //lib:\n      scope: provided\n",
            "unsupported dependency scope 'provided'",
        ),
        (
            "product: jvm/app\ndependencies:\n  - //lib:\n      extra: true\n",
            "unsupported dependency attribute 'extra'",
        ),
        (
            "product: jvm/app\ndependencies:\n  - //lib:\n    - exported\n",
            "dependency '//lib' has no attributes",
        ),
        (
            "product: jvm/app\ndependencies:\n  -\n    - //lib\n",
            "a dependency must be a module path or a Maven coordinate",
        ),
        (
            "product: jvm/app\ndependencies:\n  - //../outside\n",
            "module path '../outside' leaves the project",
        ),
    ];
    for (contents, message) in cases {
        expect_module_error(contents, message);
    }
}

#[test]
fn a_dependency_must_name_a_module_inside_the_project() {
    let tree = Temp::new("dep-path");
    tree.write("project.yaml", "modules:\n  - app\n  - lib\n");
    tree.write(
        "app/module.yaml",
        "product: jvm/app\ndependencies:\n  - ../../outside\n",
    );
    tree.write("app/src/main.kt", "fun main() {}\n");
    tree.write("lib/module.yaml", "product: jvm/lib\n");
    tree.write("lib/src/Lib.kt", "class Lib\n");
    assert_eq!(
        load(&command(&tree.0)).unwrap_err(),
        file_error(
            &tree,
            "app/module.yaml",
            "dependency '../../outside' escapes the project"
        )
    );

    tree.write(
        "app/module.yaml",
        "product: jvm/app\ndependencies:\n  - //missing\n",
    );
    assert_eq!(
        load(&command(&tree.0)).unwrap_err(),
        file_error(
            &tree,
            "app/module.yaml",
            "dependency '//missing' does not match a module in this project"
        )
    );

    tree.write(
        "app/module.yaml",
        "product: jvm/app\ndependencies:\n  - //lib\n",
    );
    let _ = std::fs::remove_dir_all(tree.0.join("lib/src"));
    tree.write("lib/test/LibTest.kt", "class LibTest\n");
    assert_eq!(
        load(&command(&tree.0)).unwrap_err(),
        "module 'app' depends on 'lib', which has no Kotlin sources"
    );
}

#[test]
fn an_empty_selected_module_and_a_dependency_cycle_are_reported() {
    let empty = Temp::new("empty-module");
    empty.write("module.yaml", "product: jvm/app\n");
    let name = empty.0.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        load(&command(&empty.0)).unwrap_err(),
        file_error(
            &empty,
            "module.yaml",
            &format!("module '{name}' has no Kotlin sources to compile")
        )
    );

    let cycle = Temp::new("cycle");
    cycle.write("project.yaml", "modules:\n  - app\n  - lib\n");
    cycle.write(
        "app/module.yaml",
        "product: jvm/app\ndependencies:\n  - //lib\n",
    );
    cycle.write("app/src/main.kt", "fun main() {}\n");
    cycle.write(
        "lib/module.yaml",
        "product: jvm/lib\ndependencies:\n  - //app\n",
    );
    cycle.write("lib/src/Lib.kt", "class Lib\n");
    let loaded = load(&command(&cycle.0)).expect("load");
    let mut graph = ModuleGraph::new();
    for module in loaded.modules {
        graph.insert(module).expect("insert");
    }
    assert_eq!(
        graph.build_order().unwrap_err().to_string(),
        "module dependency cycle among: app:main, lib:main"
    );
}

#[test]
fn discovery_stops_after_sixteen_ancestors_and_names_gradle_and_jps() {
    let deep = Temp::new("ancestors");
    deep.write("module.yaml", "product: jvm/app\n");
    deep.write("src/main.kt", "fun main() {}\n");
    let mut found = deep.0.clone();
    for index in 0..15 {
        found.push(format!("d{index}"));
    }
    std::fs::create_dir_all(&found).expect("depth");
    assert_eq!(load(&command(&found)).expect("within 16").root, deep.0);

    found.push("d15");
    std::fs::create_dir_all(&found).expect("past");
    assert_eq!(
        load(&command(&found)).unwrap_err(),
        format!(
            "no Kotlin Toolchain project found at {} or its parents (looked for module.yaml or project.yaml)",
            found.display()
        )
    );

    for marker in [
        "settings.gradle",
        "settings.gradle.kts",
        "build.gradle",
        "build.gradle.kts",
        "gradlew",
        "gradlew.bat",
    ] {
        let gradle = Temp::new("gradle-marker");
        gradle.write(marker, "");
        assert_eq!(
            load(&command(&gradle.0)).unwrap_err(),
            format!(
                "krusty-toolchain build compiles Kotlin Toolchain projects (module.yaml or project.yaml).\n\
                 {} is a Gradle project. Gradle remains a project-model extension and is not compiled by this command yet.",
                gradle.0.display()
            ),
            "marker {marker}"
        );
    }

    let both = Temp::new("gradle-and-jps");
    both.write("gradlew", "");
    both.write(".idea/modules.xml", "<project/>");
    assert_eq!(
        load(&command(&both.0)).unwrap_err(),
        format!(
            "krusty-toolchain build compiles Kotlin Toolchain projects (module.yaml or project.yaml).\n\
             {} is a Gradle project. Gradle remains a project-model extension and is not compiled by this command yet.",
            both.0.display()
        )
    );

    let idea = Temp::new("jps-child");
    idea.write(".idea/modules.xml", "<project/>");
    let child = idea.0.join("src");
    std::fs::create_dir_all(&child).expect("child");
    assert_eq!(
        load(&command(&child)).unwrap_err(),
        format!(
            "krusty-toolchain build compiles Kotlin Toolchain projects (module.yaml or project.yaml).\n\
             {} is a JetBrains .iml project. .iml support remains a project-model extension and is not compiled by this command yet.",
            idea.0.display()
        )
    );
}

#[test]
fn execute_rejects_a_missing_compiler_before_reading_the_project() {
    let missing = std::env::temp_dir().join(format!(
        "krusty-toolchain-missing-compiler-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&missing);
    let error = execute(&BuildCommand {
        directory: PathBuf::from("."),
        modules: Vec::new(),
        platforms: Vec::new(),
        variants: Vec::new(),
        compiler: missing.clone(),
    })
    .unwrap_err();
    assert_eq!(
        error,
        format!("compiler binary not found: {}", missing.display())
    );
}
