pluginManagement {
    repositories {
        gradlePluginPortal()
    }
}

rootProject.name = "krusty-gradle"

// Lock every resolved configuration, the plugins' own classpath included, when this is the build
// being run: the packaging and release builds resolve exactly the committed lockfiles. A checkout
// that includes this plugin with --include-build runs its own Gradle, whose embedded kotlin-dsl
// plugin resolves other versions; that development build is not locked.
if (gradle.parent == null) {
    gradle.beforeProject {
        buildscript.configurations.named("classpath") {
            resolutionStrategy.activateDependencyLocking()
        }
        dependencyLocking.lockAllConfigurations()
    }
}
