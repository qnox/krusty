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
