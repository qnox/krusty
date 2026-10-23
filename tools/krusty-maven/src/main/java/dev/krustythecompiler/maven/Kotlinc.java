package dev.krustythecompiler.maven;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.IOException;
import java.io.PrintStream;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.net.MalformedURLException;
import java.net.URL;
import java.net.URLClassLoader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.stream.Stream;
import org.apache.maven.plugin.MojoExecutionException;

/**
 * kotlinc of the build's Kotlin release, run in this JVM as kotlin-maven-plugin runs it. Maven tells
 * a goal nothing of how long an earlier mojo took, so the goal compiles each compilation with kotlinc
 * once more and times that; the output must equal the build's, which proves it the same compile.
 */
final class Kotlinc {
    private final URLClassLoader loader;

    Kotlinc(List<File> jars) throws MojoExecutionException {
        URL[] urls = new URL[jars.size()];
        try {
            for (int i = 0; i < urls.length; i++) urls[i] = jars.get(i).toURI().toURL();
        } catch (MalformedURLException e) {
            throw new MojoExecutionException("cannot load kotlinc from " + jars, e);
        }
        loader = new URLClassLoader(urls, ClassLoader.getPlatformClassLoader());
    }

    /**
     * Compile {@code compilation} with kotlinc and return how long it took, in milliseconds. kotlinc
     * writes to a directory of its own outside the build, compared afterwards with the build's
     * output, and removed.
     */
    long time(KotlinCompilation compilation) throws MojoExecutionException {
        Path destination;
        try {
            destination = Files.createTempDirectory("krusty-verify-kotlinc-");
        } catch (IOException e) {
            throw new MojoExecutionException("cannot create kotlinc's output directory", e);
        }
        try {
            List<String> arguments = new ArrayList<>(compilation.kotlincArguments());
            arguments.add("-d");
            arguments.add(destination.toString());
            for (File source : compilation.sources) arguments.add(source.getAbsolutePath());
            ByteArrayOutputStream messages = new ByteArrayOutputStream();
            long start = System.nanoTime();
            String exitCode = exec(arguments, new PrintStream(messages, true, StandardCharsets.UTF_8));
            long millis = (System.nanoTime() - start) / 1_000_000;
            if (!exitCode.equals("OK")) {
                throw new MojoExecutionException("kotlinc failed on " + compilation.name + ", which the build "
                        + "compiled; krusty:verify reads kotlin-maven-plugin's configuration incompletely:\n"
                        + messages.toString(StandardCharsets.UTF_8));
            }
            List<String> differing = differences(destination, compilation.reference.toPath());
            if (!differing.isEmpty()) {
                throw new MojoExecutionException("kotlinc as krusty:verify runs it wrote " + differing
                        + " of " + compilation.name + " differently from the build; krusty:verify reads "
                        + "kotlin-maven-plugin's configuration incompletely");
            }
            return millis;
        } finally {
            delete(destination);
        }
    }

    private String exec(List<String> arguments, PrintStream messages) throws MojoExecutionException {
        Thread thread = Thread.currentThread();
        ClassLoader context = thread.getContextClassLoader();
        thread.setContextClassLoader(loader);
        try {
            Class<?> compiler = Class.forName("org.jetbrains.kotlin.cli.jvm.K2JVMCompiler", true, loader);
            Method exec = compiler.getMethod("exec", PrintStream.class, String[].class);
            Object code = exec.invoke(compiler.getConstructor().newInstance(), messages,
                    arguments.toArray(new String[0]));
            return ((Enum<?>) code).name();
        } catch (InvocationTargetException e) {
            throw new MojoExecutionException("kotlinc failed", e.getCause());
        } catch (ReflectiveOperationException e) {
            throw new MojoExecutionException("cannot run kotlinc", e);
        } finally {
            thread.setContextClassLoader(context);
        }
    }

    /** The files of kotlinc's output that the build's output lacks or holds with other bytes. */
    private static List<String> differences(Path kotlinc, Path build) throws MojoExecutionException {
        List<String> differing = new ArrayList<>();
        try (Stream<Path> files = Files.walk(kotlinc)) {
            for (Path file : (Iterable<Path>) files.filter(Files::isRegularFile).sorted()::iterator) {
                Path relative = kotlinc.relativize(file);
                Path built = build.resolve(relative.toString());
                if (!Files.isRegularFile(built) || !Arrays.equals(Files.readAllBytes(file), Files.readAllBytes(built))) {
                    differing.add(relative.toString().replace(File.separatorChar, '/'));
                }
            }
        } catch (IOException e) {
            throw new MojoExecutionException("cannot compare kotlinc's output with " + build, e);
        }
        return differing;
    }

    private static void delete(Path directory) {
        try (Stream<Path> files = Files.walk(directory)) {
            for (Path path : (Iterable<Path>) files.sorted((a, b) -> b.compareTo(a))::iterator) {
                Files.deleteIfExists(path);
            }
        } catch (IOException ignored) {
            // A temporary directory left behind costs disk, not correctness.
        }
    }
}
