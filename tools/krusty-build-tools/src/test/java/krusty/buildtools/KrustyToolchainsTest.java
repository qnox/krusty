package krusty.buildtools;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.File;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import kotlin.KotlinVersion;
import org.jetbrains.kotlin.buildtools.api.BaseCompilationOperation;
import org.jetbrains.kotlin.buildtools.api.CompilationResult;
import org.jetbrains.kotlin.buildtools.api.CompilerMessageRenderer;
import org.jetbrains.kotlin.buildtools.api.KotlinLogger;
import org.jetbrains.kotlin.buildtools.api.KotlinToolchains;
import org.jetbrains.kotlin.buildtools.api.jvm.JvmPlatformToolchain;
import org.jetbrains.kotlin.buildtools.api.jvm.operations.JvmCompilationOperation;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

/**
 * Drives the implementation the way a build tool does: load it through the Build Tools API's own
 * service lookup, configure a compilation with command-line arguments, and run it in a session.
 */
class KrustyToolchainsTest {
    @TempDir
    Path work;

    private final KotlinToolchains toolchains =
        KotlinToolchains.loadImplementation(KrustyToolchainsTest.class.getClassLoader());

    @Test
    void loadsAsTheBuildToolsApiImplementationForItsKotlinRelease() {
        assertInstanceOf(KrustyToolchains.class, toolchains);
        assertEquals(KotlinVersion.CURRENT.toString(), toolchains.getCompilerVersion());
    }

    @Test
    void compilesASourceSetIntoAFreshDestination() throws IOException {
        Path source = write("Box.kt", "package p\n\nfun box(): String = \"OK\"\n");
        Path destination = work.resolve("classes");
        Path stale = destination.resolve("p/Deleted.class");
        Files.createDirectories(stale.getParent());
        Files.write(stale, new byte[0]);

        RecordingLogger logger = new RecordingLogger();
        CompilationResult result = compile(List.of(source), destination, logger, null);

        assertEquals(CompilationResult.COMPILATION_SUCCESS, result, String.join("\n", logger.errors));
        assertTrue(Files.isRegularFile(destination.resolve("p/BoxKt.class")));
        assertFalse(Files.exists(stale), "a class of a deleted source survived");
        assertEquals(List.of(), logger.errors);
    }

    @Test
    void reportsEachErrorThroughTheCallersRenderer() throws IOException {
        Path source = write("Broken.kt", "fun broken(): Int = missing()\n");
        List<String> rendered = new ArrayList<>();
        CompilerMessageRenderer renderer = (severity, message, location) -> {
            String text = severity + " " + location.getPath() + ":" + location.getLine() + ":"
                + location.getColumn() + " [" + location.getLineContent() + "] " + message;
            rendered.add(text);
            return text;
        };

        RecordingLogger logger = new RecordingLogger();
        CompilationResult result = compile(List.of(source), work.resolve("classes"), logger, renderer);

        String expected = "ERROR " + source + ":1:21 [fun broken(): Int = missing()] unresolved reference 'missing'.";
        assertEquals(CompilationResult.COMPILATION_ERROR, result);
        assertEquals(List.of(expected), rendered);
        assertEquals(List.of(expected), logger.errors);
    }

    private CompilationResult compile(
        List<Path> sources,
        Path destination,
        KotlinLogger logger,
        CompilerMessageRenderer renderer
    ) {
        JvmPlatformToolchain jvm = toolchains.getToolchain(JvmPlatformToolchain.class);
        JvmCompilationOperation.Builder builder = jvm.jvmCompilationOperationBuilder(sources, destination);
        builder.getCompilerArguments().applyArgumentStrings(List.of(
            "-classpath=" + stdlib(),
            "-no-stdlib",
            "-module-name=box"));
        if (renderer != null) {
            builder.set(BaseCompilationOperation.COMPILER_MESSAGE_RENDERER, renderer);
        }
        try (KotlinToolchains.BuildSession session = toolchains.createBuildSession()) {
            return session.executeOperation(builder.build(), toolchains.createInProcessExecutionPolicy(), logger);
        }
    }

    private static String stdlib() {
        for (String entry : System.getProperty("java.class.path").split(File.pathSeparator)) {
            if (Path.of(entry).getFileName().toString().startsWith("kotlin-stdlib-")) {
                return entry;
            }
        }
        throw new IllegalStateException("kotlin-stdlib is not on the test classpath");
    }

    private Path write(String name, String text) throws IOException {
        Path path = work.resolve(name);
        Files.writeString(path, text);
        return path;
    }

    private static final class RecordingLogger implements KotlinLogger {
        final List<String> errors = new ArrayList<>();

        @Override
        public boolean isDebugEnabled() {
            return false;
        }

        @Override
        public void error(String message, Throwable throwable) {
            errors.add(message);
        }

        @Override
        public void warn(String message, Throwable throwable) {
        }

        @Override
        public void info(String message) {
        }

        @Override
        public void debug(String message) {
        }

        @Override
        public void lifecycle(String message) {
        }
    }
}
