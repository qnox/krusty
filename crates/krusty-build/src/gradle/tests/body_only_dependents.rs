//! A dependent compile is keyed on what it observes of its classpath, not on the classpath's bytes:
//! a body-only edit in a project dependency leaves the dependent's compile up to date, and every
//! edit the dependent can observe (a signature, an inline body) recompiles it.

use super::*;

const APP_COMPILE: &str = ":app:compileKotlinWithKrusty";
const APP_CLASSPATH: &str = ":app:compileKotlinKrustyClasspath";

/// `lib` and an `app` calling into it. Each step edits `lib`, rebuilds and runs `app`, and checks
/// which of `app`'s tasks Gradle ran and what the program printed.
#[test]
#[ignore = "downloads Gradle and the Kotlin Gradle plugin"]
fn body_only_dependency_edit_keeps_dependent_compile_up_to_date() {
    let fixture = IntegrationFixture::new("krusty-body-only-dependents", write_two_projects);
    let lib = fixture.root.join("lib/src/main/kotlin/lib");
    let run = || {
        fixture
            .build()
            .tasks([":app:run"])
            .run_output()
            .unwrap_or_else(|error| panic!("{error}"))
    };

    let output = run();
    assert_eq!(greetings(&output), ["greeting=hello krusty!"], "{output}");
    assert_eq!(outcome(&output, APP_COMPILE), Outcome::Executed, "{output}");

    // A body-only edit reruns the fingerprint and leaves the compile keyed on it up to date. The
    // program prints the new body, which the unchanged app classes call into.
    write_lib(&lib, "name: String", "\"hi \" + name", "\"!\"");
    let output = run();
    assert_eq!(
        outcome(&output, APP_CLASSPATH),
        Outcome::Executed,
        "{output}"
    );
    assert_eq!(outcome(&output, APP_COMPILE), Outcome::UpToDate, "{output}");
    assert_eq!(greetings(&output), ["greeting=hi krusty!"], "{output}");

    // A public signature change is observed by the call site: the app recompiles against
    // greet(CharSequence), and runs without a NoSuchMethodError.
    write_lib(&lib, "name: CharSequence", "\"hi \" + name", "\"!\"");
    let output = run();
    assert_eq!(outcome(&output, APP_COMPILE), Outcome::Executed, "{output}");
    assert_eq!(greetings(&output), ["greeting=hi krusty!"], "{output}");

    // An inline body is copied into the app's classes, so its edit recompiles the app too, and the
    // program prints the new inlined suffix.
    write_lib(&lib, "name: CharSequence", "\"hi \" + name", "\"?\"");
    let output = run();
    assert_eq!(outcome(&output, APP_COMPILE), Outcome::Executed, "{output}");
    assert_eq!(greetings(&output), ["greeting=hi krusty?"], "{output}");

    let _ = std::fs::remove_dir_all(&fixture.root);
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Executed,
    UpToDate,
}

/// The outcome Gradle's plain console printed for `task`, which must run exactly once.
fn outcome(output: &str, task: &str) -> Outcome {
    let header = format!("> Task {task}");
    let lines = output
        .lines()
        .filter(|line| {
            *line == header
                || line
                    .strip_prefix(&header)
                    .is_some_and(|rest| rest.starts_with(' '))
        })
        .collect::<Vec<_>>();
    match lines.as_slice() {
        [line] if *line == header => Outcome::Executed,
        [line] if line.strip_prefix(&header) == Some(" UP-TO-DATE") => Outcome::UpToDate,
        _ => panic!("unexpected outcome of {task}: {lines:?}"),
    }
}

fn greetings(output: &str) -> Vec<&str> {
    output
        .lines()
        .filter(|line| line.starts_with("greeting="))
        .collect()
}

/// `greet` and the inline `shout` live in separate files: a class declaring an inline function
/// enters the fingerprint by all its bytes, so `greet`'s facade must declare none for its body-only
/// edit to stay unobserved.
fn write_lib(lib: &Path, parameter: &str, body: &str, suffix: &str) {
    let write = |file: &str, text: String| {
        let path = lib.join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write lib source");
    };
    write(
        "Greeter.kt",
        format!("package lib\n\nfun greet({parameter}): String = {body}\n"),
    );
    write(
        "Shout.kt",
        format!("package lib\n\ninline fun shout(text: String): String = text + {suffix}\n"),
    );
}

fn write_two_projects(root: &Path, plugin_version: &str, kgp_version: &str) {
    let write = |rel: &str, body: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let body = body
            .replace("PLUGIN_VERSION", plugin_version)
            .replace("KGP_VERSION", kgp_version);
        std::fs::write(path, body).expect("write");
    };
    write(
        "settings.gradle.kts",
        r#"
pluginManagement {
    repositories {
        providers.gradleProperty("krusty.plugin.repository").orNull?.let {
            maven { url = uri(it) }
        }
        gradlePluginPortal()
        mavenCentral()
    }
}

rootProject.name = "body-only-dependents"
include(":lib")
include(":app")
"#,
    );
    write(
        "build.gradle.kts",
        r#"
plugins {
    kotlin("jvm") version "KGP_VERSION" apply false
    id("dev.krustythecompiler") version "PLUGIN_VERSION" apply false
}

subprojects {
    repositories {
        mavenCentral()
    }
}
"#,
    );
    write(
        "lib/build.gradle.kts",
        r#"
plugins {
    kotlin("jvm")
    id("dev.krustythecompiler")
}
"#,
    );
    write(
        "app/build.gradle.kts",
        r#"
plugins {
    kotlin("jvm")
    id("dev.krustythecompiler")
    application
}

dependencies {
    implementation(project(":lib"))
}

application {
    mainClass.set("app.MainKt")
}
"#,
    );
    write_lib(
        &root.join("lib/src/main/kotlin/lib"),
        "name: String",
        "\"hello \" + name",
        "\"!\"",
    );
    write(
        "app/src/main/kotlin/app/Main.kt",
        "package app\n\nimport lib.greet\nimport lib.shout\n\nfun main() {\n    println(\"greeting=\" + shout(greet(\"krusty\")))\n}\n",
    );
}
