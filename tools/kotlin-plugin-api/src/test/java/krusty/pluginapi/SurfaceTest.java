package krusty.pluginapi;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.File;
import java.lang.reflect.InvocationTargetException;
import java.net.URL;
import java.net.URLClassLoader;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.stream.Stream;
import org.junit.jupiter.api.AfterAll;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.MethodSource;

/**
 * The generated surface against the real plugin jars it was derived from: every link a plugin makes
 * that the reference compiler satisfies, the surface satisfies too; every plugin class loads and
 * passes bytecode verification with only the surface and the standard library beside it; data
 * classes behave as the compiler's own, and contract members fail by naming themselves.
 */
class SurfaceTest {
    private static ClassIndex surface;
    private static ClassIndex compiler;
    private static ClassIndex stdlib;
    private static ClassLoader api;

    @BeforeAll
    static void open() throws Exception {
        surface = new ClassIndex(path("surface.jar"), false);
        compiler = new ClassIndex(path("reference.compiler"), false);
        stdlib = new ClassIndex(path("reference.stdlib"), false);
        ClassLoader base = new URLClassLoader(new URL[] {url(path("reference.stdlib"))}, ClassLoader.getPlatformClassLoader());
        api = new URLClassLoader(new URL[] {url(path("surface.jar"))}, base);
    }

    @AfterAll
    static void close() throws Exception {
        surface.close();
        compiler.close();
        stdlib.close();
    }

    static Stream<Path> plugins() {
        return Arrays.stream(System.getProperty("reference.plugins").split(File.pathSeparator)).map(Path::of);
    }

    @ParameterizedTest
    @MethodSource("plugins")
    void everyLinkTheCompilerSatisfiesTheSurfaceSatisfies(Path plugin) throws Exception {
        try (ClassIndex jar = new ClassIndex(plugin, true)) {
            LinkageCheck.Result result = new LinkageCheck(surface, compiler, stdlib).check(PluginReferences.scan(jar), jar);
            assertEquals(List.of(), result.problems());
            assertTrue(result.checked() > 0, "the plugin links against the compiler");
        }
    }

    /**
     * Loads and initializes every class with bytecode verification. An initializer may stop at a
     * member an implementation provides; what may not happen is a linkage failure, which would mean
     * the surface's shape differs from the compiler's.
     */
    @ParameterizedTest
    @MethodSource("plugins")
    void everyPluginClassLoadsAndVerifies(Path plugin) throws Exception {
        List<String> failures = new ArrayList<>();
        try (ClassIndex jar = new ClassIndex(plugin, false);
             URLClassLoader loader = new URLClassLoader(new URL[] {url(plugin)}, api)) {
            for (String name : jar.names()) {
                try {
                    Class<?> c = Class.forName(name.replace('/', '.'), true, loader);
                    c.getDeclaredMethods();
                    c.getDeclaredFields();
                } catch (ExceptionInInitializerError expected) {
                    // An initializer reached a member the surface leaves to an implementation.
                } catch (NoClassDefFoundError e) {
                    String missing = e.getMessage();
                    boolean initializerFailedEarlier = missing.startsWith("Could not initialize class");
                    boolean outsideTheCompiler = !compiler.contains(missing) && !surface.contains(missing);
                    if (!initializerFailedEarlier && !outsideTheCompiler) {
                        failures.add(name + ": " + e);
                    }
                } catch (LinkageError e) {
                    failures.add(name + ": " + e);
                }
            }
        }
        assertEquals(List.of(), failures);
    }

    @Test
    void dataClassesAreTheCompilersOwn() throws Exception {
        Class<?> name = api.loadClass("org.jetbrains.kotlin.name.Name");
        Object identifier = name.getMethod("identifier", String.class).invoke(null, "serializer");
        assertEquals("serializer", name.getMethod("asString").invoke(identifier));
        Class<?> fqName = api.loadClass("org.jetbrains.kotlin.name.FqName");
        Object fq = fqName.getConstructor(String.class).newInstance("kotlinx.serialization.Serializable");
        Object classId = api.loadClass("org.jetbrains.kotlin.name.ClassId").getMethod("topLevel", fqName).invoke(null, fq);
        assertEquals("kotlinx/serialization/Serializable", classId.getClass().getMethod("asString").invoke(classId));
    }

    @Test
    void contractMembersFailByNamingThemselves() throws Exception {
        Class<?> key = api.loadClass("org.jetbrains.kotlin.config.CompilerConfigurationKey");
        Object companion = key.getField("Companion").get(null);
        InvocationTargetException thrown = assertThrows(InvocationTargetException.class,
                () -> companion.getClass().getMethod("create", String.class).invoke(companion, "annotation"));
        assertEquals(UnsupportedOperationException.class, thrown.getCause().getClass());
        assertEquals("not provided by the Kotlin plugin API surface: org/jetbrains/kotlin/config/"
                        + "CompilerConfigurationKey$Companion.create(Ljava/lang/String;)Lorg/jetbrains/kotlin/config/CompilerConfigurationKey;",
                thrown.getCause().getMessage());
    }

    @Test
    void extensionPointsArePlainAbstractTypes() throws Exception {
        Class<?> registrar = api.loadClass("org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar");
        assertTrue(java.lang.reflect.Modifier.isAbstract(registrar.getModifiers()));
        Class<?> ir = api.loadClass("org.jetbrains.kotlin.backend.common.extensions.IrGenerationExtension");
        assertTrue(ir.isInterface());
        // The companion is the compiler's own extension-point descriptor, with its real name.
        Object companion = ir.getField("Companion").get(null);
        assertEquals("org.jetbrains.kotlin.irGenerationExtension",
                companion.getClass().getMethod("getName").invoke(companion));
    }

    private static Path path(String property) {
        return Path.of(System.getProperty(property));
    }

    private static URL url(Path p) throws Exception {
        return p.toUri().toURL();
    }
}
