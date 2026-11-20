package krusty.buildtools;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.stream.Collectors;
import java.util.stream.Stream;
import org.gradle.testkit.runner.BuildResult;
import org.gradle.testkit.runner.GradleRunner;
import org.gradle.testkit.runner.TaskOutcome;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

/**
 * The Kotlin Gradle plugin compiles a project through this implementation when it stands on the
 * plugin's Build Tools API classpath (`kotlinBuildToolsApiClasspath`) in place of
 * `kotlin-build-tools-impl`, with the plugin's own defaults otherwise: its compile task, its
 * execution strategy and its incremental compilation. The project is built by the Gradle running
 * these tests, with the Kotlin Gradle plugin of the release this jar stands in for.
 */
class KotlinGradlePluginTest {
    @TempDir
    Path work;

    @Test
    void compilesAKotlinJvmProjectThroughTheKotlinGradlePlugin() throws IOException {
        String kotlin = System.getProperty("krusty.kotlinVersion");
        Path jar = Path.of(System.getProperty("krusty.buildToolsJar"));
        Path project = work.resolve("project");
        Path log = work.resolve("krusty-invocations.txt");
        Files.createDirectories(project.resolve("src/main/kotlin/app"));
        Files.writeString(project.resolve("settings.gradle.kts"), String.join("\n",
            "pluginManagement { repositories { mavenCentral(); gradlePluginPortal() } }",
            "rootProject.name = \"app\"",
            ""));
        Files.writeString(project.resolve("build.gradle.kts"), String.join("\n",
            "plugins { kotlin(\"jvm\") version \"" + kotlin + "\" }",
            "repositories { mavenCentral() }",
            "dependencies {",
            "    kotlinBuildToolsApiClasspath(files(\"" + jar.toString().replace("\\", "/") + "\"))",
            "}",
            // The plugin adds the reference implementation and its compatibility layer, which
            // carries its own KotlinToolchains, to this classpath; krusty's replaces both.
            "configurations.kotlinBuildToolsApiClasspath {",
            "    exclude(group = \"org.jetbrains.kotlin\", module = \"kotlin-build-tools-impl\")",
            "    exclude(group = \"org.jetbrains.kotlin\", module = \"kotlin-build-tools-compat\")",
            "}",
            ""));
        Files.writeString(project.resolve("src/main/kotlin/app/Greeting.kt"),
            "package app\n\nfun greeting(): String = \"Hello\"\n");
        // A krusty that records its arguments, then compiles.
        Path recording = work.resolve("krusty");
        Files.writeString(recording, String.join("\n",
            "#!/bin/sh",
            "printf '%s\\n' \"$*\" >> '" + log + "'",
            "exec '" + System.getProperty("krusty.binary") + "' \"$@\"",
            ""));
        assertTrue(recording.toFile().setExecutable(true));

        // CI fills one Gradle user home beforehand and sets KRUSTY_GRADLE_OFFLINE: the inner build
        // reads that home and never fetches from Maven Central. Otherwise it keeps one home of its
        // own across runs, so the Kotlin Gradle plugin is downloaded once.
        boolean offline = !System.getenv().getOrDefault("KRUSTY_GRADLE_OFFLINE", "").isEmpty();
        String warmedHome = System.getenv("GRADLE_USER_HOME");
        assertTrue(!offline || warmedHome != null, "KRUSTY_GRADLE_OFFLINE needs the warmed GRADLE_USER_HOME");
        Path testKit = Path.of(offline ? warmedHome : System.getProperty("krusty.testKitDir"));
        List<String> arguments = new ArrayList<>(List.of(
            "compileKotlin", "--stacktrace", "-Dkrusty.binary=" + recording));
        if (offline) {
            arguments.add("--offline");
        }
        BuildResult result = GradleRunner.create()
            .withTestKitDir(testKit.toFile())
            .withProjectDir(project.toFile())
            // The Gradle daemon reads the binary from this system property before KRUSTY_BIN.
            .withArguments(arguments)
            .withEnvironment(Map.of(
                "PATH", System.getenv("PATH"),
                "HOME", System.getProperty("user.home")))
            .build();

        assertEquals(TaskOutcome.SUCCESS, result.task(":compileKotlin").getOutcome(), result.getOutput());
        try (Stream<Path> classes = Files.walk(project.resolve("build/classes/kotlin/main"))) {
            assertEquals(
                List.of("app/GreetingKt.class"),
                classes.filter(Files::isRegularFile)
                    .map(path -> project.resolve("build/classes/kotlin/main").relativize(path).toString())
                    .filter(name -> name.endsWith(".class"))
                    .sorted()
                    .collect(Collectors.toList()));
        }
        // The compile went through krusty. The plugin fingerprints the compilation, compiles, and
        // fingerprints again for its incremental state. It names the source in its arguments and
        // again as the operation's source, which krusty compiles once.
        List<String> invocations = Files.readAllLines(log);
        assertEquals(
            List.of(true, false, true),
            invocations.stream()
                .map(line -> line.startsWith("compilation-fingerprint "))
                .collect(Collectors.toList()),
            String.join("\n", invocations));
        String compile = invocations.get(1);
        assertTrue(compile.startsWith("-Xallow-no-source-files "), compile);
        assertTrue(compile.contains(" -d " + project.toRealPath() + "/build/classes/kotlin/main "), compile);
        assertEquals(2, compile.split("Greeting\\.kt", -1).length - 1, compile);
    }
}
