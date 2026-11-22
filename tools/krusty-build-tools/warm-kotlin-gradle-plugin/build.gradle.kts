// Applies the Kotlin Gradle plugin so that building this project fills the Gradle cache with
// everything the KotlinGradlePluginTest inner build resolves. It is built by `warmKotlinGradlePlugin`
// and has no sources of its own.
plugins {
    kotlin("jvm")
}

repositories {
    mavenCentral()
}
