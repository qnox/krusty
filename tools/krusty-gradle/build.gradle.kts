import org.gradle.api.publish.maven.MavenPublication
import org.gradle.api.tasks.Sync
import org.gradle.api.tasks.bundling.AbstractArchiveTask

plugins {
    `kotlin-dsl`
    `maven-publish`
}

group = "dev.krusty"
version = providers.gradleProperty("krustyPluginVersion").orElse("0.0.1").get()

repositories {
    mavenCentral()
}

dependencies {
    // Compile against the oldest supported public API. The consuming build supplies and applies
    // its selected Kotlin/JVM plugin; the release JAR does not bundle a Kotlin Gradle plugin.
    compileOnly("org.jetbrains.kotlin:kotlin-gradle-plugin-api:2.4.0")
}

// Package the compiler's vendored tables separately. The plugin selects the table for the exact
// Kotlin Gradle plugin version; merging releases would let an option from one release consume a
// plugin-owned token under another release's grammar.
val kotlincArgumentResources = layout.buildDirectory.dir("generated/kotlinc-arguments")
val kotlincArgumentTables = tasks.register<Sync>("kotlincArgumentTables") {
    from("../../crates/krusty-cli/src/kotlinc_arguments/releases") {
        include("*.tsv")
        exclude("*.features.tsv")
    }
    into(kotlincArgumentResources.map { it.dir("krusty/kotlinc-arguments") })
}
sourceSets.main {
    resources.srcDir(files(kotlincArgumentResources).builtBy(kotlincArgumentTables))
}

gradlePlugin {
    plugins {
        create("krusty") {
            id = "krusty"
            implementationClass = "krusty.KrustyKotlinPlugin"
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
