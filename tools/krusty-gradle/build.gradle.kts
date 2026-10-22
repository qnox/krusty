import org.gradle.api.publish.maven.MavenPublication
import org.gradle.api.tasks.Sync
import org.gradle.api.tasks.bundling.AbstractArchiveTask

plugins {
    `kotlin-dsl`
    `maven-publish`
    signing
    id("com.gradle.plugin-publish") version "1.3.1"
}

group = "dev.krustythecompiler"
version = providers.gradleProperty("krustyPluginVersion").orElse("0.0.1").get()

repositories {
    mavenCentral()
}

dependencies {
    // Compile against the oldest supported public API. The consuming build supplies and applies
    // its selected Kotlin/JVM plugin; the release JAR does not bundle a Kotlin Gradle plugin.
    compileOnly("org.jetbrains.kotlin:kotlin-gradle-plugin-api:2.4.0")
    testImplementation(kotlin("test-junit5"))
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

tasks.test {
    useJUnitPlatform()
}

// Package the compiler's vendored tables separately, with the manifest of supported versions. The
// plugin selects the table the exact Kotlin Gradle plugin version reads; merging releases would let
// an option from one release consume a plugin-owned token under another release's grammar.
val kotlincArgumentResources = layout.buildDirectory.dir("generated/kotlinc-arguments")
val kotlincArgumentTables = tasks.register<Sync>("kotlincArgumentTables") {
    from("../../crates/krusty-cli/src/kotlinc_arguments/releases") {
        include("*.tsv")
        exclude("*.features.tsv")
        into("krusty/kotlinc-arguments")
    }
    from("../../kotlin-versions") {
        into("krusty")
    }
    into(kotlincArgumentResources)
}
sourceSets.main {
    resources.srcDir(files(kotlincArgumentResources).builtBy(kotlincArgumentTables))
}

gradlePlugin {
    website = "https://krustythecompiler.dev"
    vcsUrl = "https://github.com/qnox/krusty"
    plugins {
        create("krusty") {
            id = "dev.krustythecompiler"
            implementationClass = "krusty.KrustyKotlinPlugin"
            displayName = "krusty"
            description = "Compiles Kotlin/JVM source sets with krusty, a Kotlin compiler written in Rust that " +
                "emits kotlinc's bytecode; a compile it does not support stays with kotlinc."
            tags = listOf("kotlin", "compiler", "kotlinc", "krusty", "build-performance")
        }
    }
}

tasks.jar {
    archiveFileName.set("krusty-gradle-plugin-${project.version}.jar")
    manifest.attributes(
        "Implementation-Title" to "krusty Gradle plugin",
        "Implementation-Version" to project.version,
    )
}

tasks.withType<AbstractArchiveTask>().configureEach {
    isPreserveFileTimestamps = false
    isReproducibleFileOrder = true
}

publishing {
    publications.withType<MavenPublication>().configureEach {
        if (name == "pluginMaven") artifactId = "krusty-gradle-plugin"
    }
    repositories {
        maven {
            name = "release"
            url = layout.buildDirectory.dir("release-repository").get().asFile.toURI()
        }
    }
}

// Releases are signed with the maintainer's krusty signing subkey, passed in memory by the release
// job; a local or CI build without the key publishes unsigned.
signing {
    val key = providers.gradleProperty("signingInMemoryKey").orNull
    if (key != null) {
        useInMemoryPgpKeys(
            providers.gradleProperty("signingInMemoryKeyId").orNull,
            key,
            providers.gradleProperty("signingInMemoryKeyPassword").orNull,
        )
    }
    isRequired = key != null
    sign(publishing.publications)
}
