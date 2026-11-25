//! The Kotlin compiler slice: modules laid out like the Kotlin repository's, which the compiler-slice
//! integration test and its compiler-option scenarios build.

use super::*;

pub(super) fn write_compiler_slice(root: &Path, plugin_version: &str, kgp_version: &str) {
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

rootProject.name = "kotlin-compiler-slice"
include(":core:util.runtime")
include(":compiler:util")
"#,
    );
    // The Kotlin Gradle plugin is requested once, at the root: Kotlin 2.5 fails a build that
    // loads it in several subprojects' classloaders.
    write(
        "build.gradle.kts",
        r#"
plugins {
    kotlin("jvm") version "KGP_VERSION" apply false
    kotlin("plugin.assignment") version "KGP_VERSION" apply false
}

subprojects {
    repositories {
        mavenCentral()
    }
}
"#,
    );
    write(
        "core/util.runtime/build.gradle.kts",
        r#"
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.JvmDefaultMode
import org.jetbrains.kotlin.gradle.dsl.KotlinVersion
import org.jetbrains.kotlin.gradle.tasks.KotlinJvmCompile
import org.gradle.api.tasks.compile.JavaCompile

plugins {
    kotlin("jvm")
    id("dev.krustythecompiler") version "PLUGIN_VERSION"
    `java-library`
}

val krustyNegative = providers.gradleProperty("krusty.negative").orNull

// A source directory added by path, its generator wired onto the Kotlin task alone.
val generateStamp by tasks.registering {
    val out = layout.buildDirectory.file("generated/stamp/org/jetbrains/kotlin/util/Stamp.kt")
    outputs.file(out)
    doLast {
        out.get().asFile.apply {
            parentFile.mkdirs()
            writeText("package org.jetbrains.kotlin.util\nconst val STAMP = \"stamp\"\n")
        }
    }
}

kotlin {
    sourceSets.named("main") {
        kotlin.srcDir("src")
        kotlin.srcDir(layout.buildDirectory.dir("generated/stamp"))
    }
}

tasks.named("compileKotlin") { dependsOn(generateStamp) }

sourceSets.named("main") {
    java.setSrcDirs(listOf("src"))
}

tasks.withType<KotlinJvmCompile>().configureEach {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
        moduleName.set("kotlin-util-runtime")
        languageVersion.set(KotlinVersion.KOTLIN_2_4)
        apiVersion.set(KotlinVersion.KOTLIN_2_4)
        jvmDefault.set(JvmDefaultMode.NO_COMPATIBILITY)
        javaParameters.set(true)
        freeCompilerArgs.add("-Xconsistent-data-class-copy-visibility")
    }
}

tasks.withType<JavaCompile>().configureEach {
    options.release.set(if (krustyNegative == "jvm-target") 11 else 17)
}
"#,
    );
    write(
        "core/util.runtime/src/org/jetbrains/kotlin/util/Runtime.kt",
        "package org.jetbrains.kotlin.util\nfun runtimeMarker() = \"runtime\"\n",
    );
    write(
        "core/util.runtime/src/org/jetbrains/kotlin/util/RuntimeJava.java",
        "package org.jetbrains.kotlin.util;\npublic class RuntimeJava { public static String marker() { return \"java\"; } }\n",
    );
    write(
        "compiler/util/build.gradle.kts",
        r#"
@file:OptIn(
    org.jetbrains.kotlin.buildtools.api.ExperimentalBuildToolsApi::class,
    org.jetbrains.kotlin.gradle.ExperimentalKotlinGradlePluginApi::class,
)

import org.jetbrains.kotlin.gradle.dsl.JvmDefaultMode
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.KotlinVersion
import org.jetbrains.kotlin.gradle.tasks.KotlinJvmCompile
import org.gradle.api.DefaultTask
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.tasks.OutputDirectory
import org.gradle.api.tasks.TaskAction
import org.gradle.api.tasks.compile.JavaCompile
import java.nio.file.Files
import java.nio.file.LinkOption

plugins {
    kotlin("jvm")
    id("dev.krustythecompiler") version "PLUGIN_VERSION"
    `java-library`
}

dependencies {
    api(project(":core:util.runtime"))
}

val krustyNegative = providers.gradleProperty("krusty.negative").orNull

if (krustyNegative == "compiler-plugin") {
    pluginManager.apply("org.jetbrains.kotlin.plugin.assignment")
}

abstract class GenerateKotlin : DefaultTask() {
    @get:OutputDirectory
    abstract val outputDirectory: DirectoryProperty

    @TaskAction
    fun generate() {
        outputDirectory.get().file("org/jetbrains/kotlin/util/Generated.kt").asFile.apply {
            parentFile.mkdirs()
            writeText("package org.jetbrains.kotlin.util\nclass Generated\n")
        }
    }
}

val generateKotlin = tasks.register<GenerateKotlin>("generateKotlin") {
    outputDirectory.set(layout.buildDirectory.dir("generated/krusty"))
}

kotlin {
    sourceSets.named("main") {
        kotlin.srcDir("src")
        kotlin.srcDir("other-src")
        kotlin.srcDir(generateKotlin.flatMap { it.outputDirectory })
    }
    sourceSets.named("test") {
        kotlin.srcDir("test-src")
    }
}

sourceSets.named("main") {
    java.setSrcDirs(listOf("src"))
    resources.srcDir("resources")
}

tasks.withType<KotlinJvmCompile>().configureEach {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
        moduleName.set("kotlin-compiler-util")
        languageVersion.set(KotlinVersion.KOTLIN_2_4)
        apiVersion.set(KotlinVersion.KOTLIN_2_4)
        jvmDefault.set(JvmDefaultMode.NO_COMPATIBILITY)
        javaParameters.set(true)
        optIn.set(
            listOf(
                "krusty.fixture.ExperimentalFirstApi",
                "krusty.fixture.ExperimentalSecondApi",
            ),
        )
        freeCompilerArgs.add("-Xconsistent-data-class-copy-visibility")
        freeCompilerArgs.add("-Xskip-prerelease-check")
        when (krustyNegative) {
            "structured-option" -> extraWarnings.set(true)
            "all-warnings-as-errors" -> allWarningsAsErrors.set(true)
            "plugin-free-argument" -> freeCompilerArgs.add("-Xplugin=forbidden.jar")
            "explicit-api-bad-mode" -> freeCompilerArgs.add("-Xexplicit-api=sideways")
            "old-api-version" -> apiVersion.set(KotlinVersion.fromVersion("1.9"))
            "module-name-free-argument" -> freeCompilerArgs.add("-module-name=overridden")
            "release-specific-argument-boundary" -> freeCompilerArgs.addAll(
                "-Xescaping-functions",
                "-d=forbidden",
            )
            "jspecify-free-argument" -> freeCompilerArgs.add("-Xjspecify-annotations=strict")
        }
    }
}

tasks.withType<JavaCompile>().configureEach {
    options.release.set(17)
}

// A source set with Java sources only, at another Java target: its Kotlin task has no source.
if (krustyNegative == "java-only-source-set") {
    val javaOnly = sourceSets.create("javaOnly") { java.setSrcDirs(listOf("java-only-src")) }
    tasks.named<JavaCompile>(javaOnly.compileJavaTaskName) { options.release.set(11) }
}

// The Kotlin task's own Java toolchain: the build's JDK, declared as another release, so the
// build needs no second JDK. Gradle answers its own JDK home with that JDK's release, so the JDK
// is reached through a home of links.
if (krustyNegative == "kotlin-java-toolchain") {
    logger.lifecycle("fixture: Gradle Java " + JavaVersion.current().majorVersion)
    val linkedJdk = layout.buildDirectory.dir("linked-jdk").get().asFile
    linkedJdk.mkdirs()
    File(System.getProperty("java.home")).listFiles().orEmpty().forEach { entry ->
        val link = linkedJdk.resolve(entry.name).toPath()
        if (!Files.exists(link, LinkOption.NOFOLLOW_LINKS)) {
            Files.createSymbolicLink(link, entry.toPath())
        }
    }
    tasks.named<KotlinJvmCompile>("compileKotlin") {
        kotlinJavaToolchain.jdk.use(
            linkedJdk,
            JavaVersion.toVersion(JavaVersion.current().majorVersion.toInt() - 1),
        )
        logger.lifecycle("fixture: Kotlin task Java " + kotlinJavaToolchain.javaVersion.get().majorVersion)
    }
}
"#,
    );
    write(
        "compiler/util/java-only-src/org/jetbrains/kotlin/util/JavaOnlySourceSet.java",
        "package org.jetbrains.kotlin.util;\npublic class JavaOnlySourceSet {}\n",
    );
    write(
        "compiler/util/src/org/jetbrains/kotlin/util/Util.kt",
        "package org.jetbrains.kotlin.util\nfun compilerMarker() = runtimeMarker() + RuntimeJava.marker() + JavaOnly.marker()\n",
    );
    write(
        "compiler/util/src/org/jetbrains/kotlin/util/Other.kt",
        "package org.jetbrains.kotlin.util\ninternal fun otherMarker() = 1\n",
    );
    write(
        "compiler/util/src/first/Same.kt",
        "package first\nclass Same\n",
    );
    write(
        "compiler/util/other-src/second/Same.kt",
        "package second\nclass Same\n",
    );
    write(
        "compiler/util/test-src/org/jetbrains/kotlin/util/Friend.kt",
        "package org.jetbrains.kotlin.util\nfun friendMarker() = otherMarker()\n",
    );
    write(
        "compiler/util/src/org/jetbrains/kotlin/util/Skip.kts",
        "println(\"script\")\n",
    );
    write(
        "compiler/util/src/org/jetbrains/kotlin/util/JavaOnly.java",
        "package org.jetbrains.kotlin.util;\npublic class JavaOnly { public static int marker() { return 1; } }\n",
    );
    write(
        "compiler/util/src/org/jetbrains/kotlin/util/JavaUsesKotlin.java",
        "package org.jetbrains.kotlin.util;\npublic class JavaUsesKotlin { public static String marker() { return UtilKt.compilerMarker(); } }\n",
    );
    write("compiler/util/resources/notice.txt", "notice\n");
}
