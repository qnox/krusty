plugins {
    `java-library`
}

group = "dev.krusty"
version = providers.gradleProperty("krustyAnalysisApiVersion").orElse("0.0.1").get()

// The KSP release whose embedded Analysis API this adapter is built against. KSP2 ships the
// Analysis API of the Kotlin release it targets, with every package moved under `ksp.`; the
// adapter is written against those names.
val kspVersion = providers.gradleProperty("kspVersion").orElse("2.3.14")

repositories {
    mavenCentral()
}

// The Kotlin release KSP builds the Analysis API with: its signatures take Kotlin's own function
// and unit types, which the embeddable jar leaves unrelocated.
val kotlinVersion = providers.gradleProperty("kotlinVersion").orElse("2.3.20")

dependencies {
    compileOnly(kotlinVersion.map { "org.jetbrains.kotlin:kotlin-stdlib:$it" })
    compileOnly(kspVersion.map { "com.google.devtools.ksp:symbol-processing-aa-embeddable:$it" })

    testImplementation(kotlinVersion.map { "org.jetbrains.kotlin:kotlin-stdlib:$it" })
    testImplementation(kspVersion.map { "com.google.devtools.ksp:symbol-processing-aa-embeddable:$it" })
    testImplementation(platform("org.junit:junit-bom:5.13.4"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

tasks.withType<JavaCompile>().configureEach {
    options.release.set(17)
}

tasks.test {
    useJUnitPlatform()
    jvmArgs("-Xmx2g")
}
