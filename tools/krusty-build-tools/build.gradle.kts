import org.gradle.api.tasks.bundling.AbstractArchiveTask

plugins {
    `java-library`
}

group = "dev.krusty"
version = providers.gradleProperty("krustyBuildToolsVersion").orElse("0.0.1").get()

// The one Kotlin release this implementation stands in for: a build tool loads the Build Tools API
// implementation that matches its configured Kotlin version, and krusty compiles as that release.
val kotlinVersion = providers.gradleProperty("kotlinVersion").orElse("2.4.20")

repositories {
    mavenCentral()
}

dependencies {
    // The caller's class loader supplies the Build Tools API; the implementation must not bundle it.
    compileOnly(kotlinVersion.map { "org.jetbrains.kotlin:kotlin-build-tools-api:$it" })

    testImplementation(kotlinVersion.map { "org.jetbrains.kotlin:kotlin-build-tools-api:$it" })
    testImplementation(kotlinVersion.map { "org.jetbrains.kotlin:kotlin-stdlib:$it" })
    testImplementation(platform("org.junit:junit-bom:5.13.4"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

tasks.withType<JavaCompile>().configureEach {
    options.release.set(17)
}

val versionResource = tasks.register("versionResource") {
    val output = layout.buildDirectory.dir("generated/version-resource")
    val version = kotlinVersion
    inputs.property("kotlinVersion", version)
    outputs.dir(output)
    doLast {
        val file = output.get().file("krusty/buildtools/krusty-build-tools.properties").asFile
        file.parentFile.mkdirs()
        file.writeText("kotlin.version=${version.get()}\n")
    }
}

tasks.test {
    useJUnitPlatform()
    // The tests drive a real krusty binary: -Pkrusty.binary=<path> or KRUSTY_BIN.
    val binary = providers.gradleProperty("krusty.binary").orElse(providers.environmentVariable("KRUSTY_BIN"))
    inputs.property("krustyBinary", binary.orElse(""))
    doFirst {
        systemProperty("krusty.binary", binary.orNull ?: throw GradleException("set -Pkrusty.binary or KRUSTY_BIN"))
    }
}

sourceSets.main {
    resources.srcDir(versionResource)
}

tasks.jar {
    archiveFileName.set("krusty-build-tools-${project.version}-kotlin-${kotlinVersion.get()}.jar")
    manifest.attributes(
        "Implementation-Title" to "krusty Kotlin Build Tools API implementation",
        "Implementation-Version" to project.version,
    )
}

tasks.withType<AbstractArchiveTask>().configureEach {
    isPreserveFileTimestamps = false
    isReproducibleFileOrder = true
}
