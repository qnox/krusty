import org.gradle.api.publish.maven.MavenPublication
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

// The plugin reads the compiler's own vendored kotlinc argument tables, so it resolves the same
// short and deprecated spellings the compiler does without keeping a copy of its own.
val kotlincArgumentTables = fileTree("../../crates/krusty-cli/src/kotlinc_arguments/releases") {
    include("*.tsv")
    exclude("*.features.tsv")
}
val kotlincArgumentResources = layout.buildDirectory.dir("generated/kotlinc-arguments")
val kotlincArgumentTable = tasks.register("kotlincArgumentTable") {
    val tables = kotlincArgumentTables
    val output = kotlincArgumentResources.map { it.file("krusty/kotlinc-arguments.tsv") }
    inputs.files(tables)
    outputs.file(output)
    doLast {
        val text = tables.files.sortedBy { it.name }.joinToString("") { it.readText() }
        output.get().asFile.apply { parentFile.mkdirs() }.writeText(text)
    }
}
sourceSets.main {
    resources.srcDir(files(kotlincArgumentResources).builtBy(kotlincArgumentTable))
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
