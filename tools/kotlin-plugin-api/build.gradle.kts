import org.gradle.api.tasks.bundling.AbstractArchiveTask

plugins {
    java
}

group = "dev.krusty"

// The one Kotlin release the surface is derived from. Plugins are built against an exact compiler,
// so the surface is regenerated per release rather than maintained by hand.
val kotlinVersion = providers.gradleProperty("kotlinVersion").orElse("2.4.20")

// The plugins whose links define the surface: every first-party plugin Gradle builds load (their
// -embeddable variants, which link against the shaded kotlin-compiler-embeddable), plus two
// independent ones that exercise the API more widely than JetBrains' own.
val firstPartyPlugins = listOf(
    "kotlin-allopen-compiler-plugin-embeddable",
    "kotlin-noarg-compiler-plugin-embeddable",
    "kotlin-sam-with-receiver-compiler-plugin-embeddable",
    "kotlin-assignment-compiler-plugin-embeddable",
    "kotlin-lombok-compiler-plugin-embeddable",
    "kotlin-power-assert-compiler-plugin-embeddable",
    "kotlin-parcelize-compiler",
    "kotlin-serialization-compiler-plugin-embeddable",
    "kotlin-atomicfu-compiler-plugin-embeddable",
    "kotlin-compose-compiler-plugin-embeddable",
)
val thirdPartyPlugins = listOf(
    "dev.drewhamilton.poko:poko-compiler-plugin:0.23.2",
    "dev.zacsweers.metro:compiler:1.4.5",
)

repositories {
    mavenCentral()
}

val referenceCompiler: Configuration by configurations.creating { isTransitive = false }
val referenceStdlib: Configuration by configurations.creating { isTransitive = false }
val referencePlugins: Configuration by configurations.creating { isTransitive = false }

dependencies {
    implementation("org.ow2.asm:asm:9.7.1")
    implementation("org.ow2.asm:asm-tree:9.7.1")

    referenceCompiler(kotlinVersion.map { "org.jetbrains.kotlin:kotlin-compiler-embeddable:$it" })
    referenceStdlib(kotlinVersion.map { "org.jetbrains.kotlin:kotlin-stdlib:$it" })
    for (plugin in firstPartyPlugins) {
        referencePlugins(kotlinVersion.map { "org.jetbrains.kotlin:$plugin:$it" })
    }
    for (plugin in thirdPartyPlugins) {
        referencePlugins(plugin)
    }

    testImplementation(platform("org.junit:junit-bom:5.13.4"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

tasks.withType<JavaCompile>().configureEach {
    options.release.set(21)
}

val surfaceFile = layout.buildDirectory.zip(kotlinVersion) { dir, v -> dir.file("libs/kotlin-plugin-api-surface-$v.jar") }
val reportFile = layout.buildDirectory.zip(kotlinVersion) { dir, v -> dir.file("reports/kotlin-plugin-api-surface-$v.md") }

val surfaceJar = tasks.register<JavaExec>("surfaceJar") {
    description = "Derives the Kotlin compiler plugin API surface jar from the reference plugins."
    classpath = sourceSets.main.get().runtimeClasspath
    mainClass.set("krusty.pluginapi.SurfaceGenerator")
    maxHeapSize = "3g"
    val compiler = referenceCompiler
    val stdlib = referenceStdlib
    val plugins = referencePlugins
    val version = kotlinVersion
    val out = surfaceFile
    val report = reportFile
    inputs.files(compiler, stdlib, plugins)
    inputs.property("kotlinVersion", version)
    outputs.file(out)
    outputs.file(report)
    argumentProviders.add(CommandLineArgumentProvider {
        listOf(
            "--compiler", compiler.singleFile.path,
            "--stdlib", stdlib.singleFile.path,
            "--kotlin-version", version.get(),
            "--out", out.get().asFile.path,
            "--report", report.get().asFile.path,
        ) + plugins.files.map { it.path }
    })
}

tasks.assemble {
    dependsOn(surfaceJar)
}

tasks.test {
    useJUnitPlatform()
    dependsOn(surfaceJar)
    maxHeapSize = "3g"
    val compiler = referenceCompiler
    val stdlib = referenceStdlib
    val plugins = referencePlugins
    val surface = surfaceFile
    inputs.files(compiler, stdlib, plugins)
    inputs.file(surface)
    doFirst {
        systemProperty("surface.jar", surface.get().asFile.path)
        systemProperty("reference.compiler", compiler.singleFile.path)
        systemProperty("reference.stdlib", stdlib.singleFile.path)
        systemProperty("reference.plugins", plugins.files.joinToString(File.pathSeparator) { it.path })
    }
}

tasks.withType<AbstractArchiveTask>().configureEach {
    isPreserveFileTimestamps = false
    isReproducibleFileOrder = true
}
