package dev.krustythecompiler.maven;

import java.io.BufferedReader;
import java.io.File;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicInteger;
import org.apache.maven.artifact.DependencyResolutionRequiredException;
import org.apache.maven.execution.MavenSession;
import org.apache.maven.model.Dependency;
import org.apache.maven.model.Plugin;
import org.apache.maven.plugin.AbstractMojo;
import org.apache.maven.plugin.MojoExecutionException;
import org.apache.maven.plugins.annotations.Component;
import org.apache.maven.plugins.annotations.Mojo;
import org.apache.maven.plugins.annotations.Parameter;
import org.apache.maven.plugins.annotations.ResolutionScope;
import org.apache.maven.project.MavenProject;
import org.apache.maven.rtinfo.RuntimeInformation;
import org.eclipse.aether.RepositorySystem;
import org.eclipse.aether.SessionData;
import org.eclipse.aether.artifact.DefaultArtifact;
import org.eclipse.aether.collection.CollectRequest;
import org.eclipse.aether.resolution.ArtifactResult;
import org.eclipse.aether.resolution.DependencyRequest;
import org.eclipse.aether.resolution.DependencyResolutionException;

/**
 * Compiles each of the build's kotlin-maven-plugin compilations with krusty beside kotlinc's output,
 * and once the last project is done writes a report of what krusty compiles, how its classes differ
 * from kotlinc's, and how long it took: {@code mvn test-compile krusty:verify}. kotlinc's classes are
 * the build's output and stay untouched; krusty writes under {@code target/krusty-verify}.
 */
@Mojo(name = "verify", requiresDependencyResolution = ResolutionScope.TEST, threadSafe = true)
public class VerifyMojo extends AbstractMojo {
    @Parameter(defaultValue = "${project}", readonly = true, required = true)
    private MavenProject project;

    @Parameter(defaultValue = "${session}", readonly = true, required = true)
    private MavenSession session;

    /** A krusty executable to run instead of the plugin release's, downloaded otherwise. */
    @Parameter(property = "krusty.binary")
    private File binary;

    /** Where krusty releases are downloaded from. */
    @Parameter(property = "krusty.releases", defaultValue = KrustyRelease.GITHUB_RELEASES)
    private String releases;

    /** Keep compilation names in the issue text, which is anonymized otherwise. */
    @Parameter(property = "krusty.verify.shareNames", defaultValue = "false")
    private boolean shareNames;

    @Component
    private RepositorySystem repositorySystem;

    @Component
    private RuntimeInformation runtime;

    /** What this build's executions of the goal share: the records they wrote, and how many ran. */
    private static final class Verification {
        final Set<File> records = ConcurrentHashMap.newKeySet();
        final AtomicInteger finished = new AtomicInteger();
        /** kotlinc by Kotlin release, loaded once so later compilations run it warm, as the build does. */
        final Map<String, Kotlinc> kotlinc = new ConcurrentHashMap<>();
    }

    private static final String VERIFICATION = Verification.class.getName();

    @Override
    public void execute() throws MojoExecutionException {
        File records = new File(session.getTopLevelProject().getBuild().getDirectory(), "krusty-verify/records");
        Verification verification = verification();
        Plugin kotlin = project.getBuild().getPluginsAsMap().get(KotlinCompilation.KOTLIN_PLUGIN);
        if (kotlin != null) {
            File krusty = krusty();
            List<File> pluginClasspath = pluginClasspath(kotlin);
            String jdkHome = System.getProperty("java.home");
            try {
                for (KotlinCompilation compilation : KotlinCompilation.of(project, kotlin, "compile",
                        project.getCompileClasspathElements(), pluginClasspath, jdkHome)) {
                    verify(krusty, records, verification, compilation);
                }
                for (KotlinCompilation compilation : KotlinCompilation.of(project, kotlin, "test-compile",
                        project.getTestClasspathElements(), pluginClasspath, jdkHome)) {
                    verify(krusty, records, verification, compilation);
                }
            } catch (DependencyResolutionRequiredException e) {
                throw new MojoExecutionException("cannot read " + project.getArtifactId() + "'s classpath", e);
            }
        }
        // The last project to finish, in whatever order a parallel build runs them, writes the
        // report from exactly this build's records.
        if (verification.finished.incrementAndGet() == session.getProjects().size()) {
            report(records, verification.records);
        }
    }

    private Verification verification() {
        SessionData data = session.getRepositorySession().getData();
        data.set(VERIFICATION, null, new Verification());
        return (Verification) data.get(VERIFICATION);
    }

    private void verify(File krusty, File records, Verification verification, KotlinCompilation compilation)
            throws MojoExecutionException {
        if (compilation.hasKotlinSources() && !compilation.reference.isDirectory()) {
            throw new MojoExecutionException(compilation.reference + " holds no kotlinc output; run "
                    + "test-compile in the same build, as in 'mvn test-compile krusty:verify'");
        }
        // Nothing is deleted or written here: verify-compile proves its destination and record apart
        // from kotlinc's output before it clears or writes either.
        File destination = new File(project.getBuild().getDirectory(), "krusty-verify/" + compilation.outputName + "/classes");
        List<String> command = new ArrayList<>();
        command.add(krusty.getAbsolutePath());
        command.add("verify-compile");
        command.add("--kotlin-outputs-only");
        command.add("--reference");
        command.add(compilation.reference.getAbsolutePath());
        File record = new File(records, compilation.name.replace(':', '_').replace('@', '_') + ".json");
        command.add("--record");
        command.add(record.getAbsolutePath());
        command.add("--compilation");
        command.add(compilation.name);
        command.add("--source-set");
        command.add(compilation.sourceSet);
        command.add("--kotlin-version");
        command.add(compilation.kotlinVersion);
        if (compilation.hasKotlinSources() && compilation.unsupported.isEmpty()) {
            command.add("--kotlinc-millis");
            command.add(Long.toString(kotlinc(verification, compilation.kotlinVersion).time(compilation)));
        }
        File reasons = null;
        if (!compilation.unsupported.isEmpty()) {
            // Outside the build's directories, which verify-compile has not checked yet.
            reasons = temporary(String.join("\n", compilation.unsupported) + "\n");
            command.add("--unsupported-from");
            command.add(reasons.getAbsolutePath());
        }
        command.add("--");
        command.addAll(compilation.arguments);
        command.add("-d");
        command.add(destination.getAbsolutePath());
        for (File source : compilation.sources) command.add(source.getAbsolutePath());
        getLog().info("krusty: verifying " + compilation.name);
        try {
            run(command);
        } finally {
            if (reasons != null) reasons.delete();
        }
        verification.records.add(record);
    }

    private void report(File records, Set<File> current) throws MojoExecutionException {
        File out = new File(records.getParentFile(), "report");
        if (current.isEmpty()) {
            // An earlier build's report must not stand for this one.
            for (String name : List.of("krusty-verify.html", "krusty-verify.md", "krusty-verify-issue.md")) {
                new File(out, name).delete();
            }
            getLog().warn("krusty: no kotlin-maven-plugin compilation ran, so there is nothing to report");
            return;
        }
        List<String> command = new ArrayList<>();
        command.add(krusty().getAbsolutePath());
        command.add("verify-report");
        command.add("--out");
        command.add(out.getAbsolutePath());
        command.add("--build-tool");
        command.add("Maven " + runtime.getMavenVersion());
        command.add("--jdk");
        command.add(System.getProperty("java.version"));
        if (shareNames) command.add("--share-names");
        current.stream().map(File::getAbsolutePath).sorted().forEach(command::add);
        run(command);
    }

    private Kotlinc kotlinc(Verification verification, String version) throws MojoExecutionException {
        Kotlinc loaded = verification.kotlinc.get(version);
        if (loaded != null) return loaded;
        synchronized (verification.kotlinc) {
            loaded = verification.kotlinc.get(version);
            if (loaded == null) {
                CollectRequest collect = new CollectRequest(new org.eclipse.aether.graph.Dependency(
                        new DefaultArtifact("org.jetbrains.kotlin", "kotlin-compiler-embeddable", "jar", version),
                        "runtime"), project.getRemotePluginRepositories());
                loaded = new Kotlinc(resolve(collect, "kotlinc " + version));
                verification.kotlinc.put(version, loaded);
            }
            return loaded;
        }
    }

    /** The resolved jars of kotlin-maven-plugin's own dependencies, which carry its compiler plugins. */
    private List<File> pluginClasspath(Plugin kotlin) throws MojoExecutionException {
        if (kotlin.getDependencies().isEmpty()) return new ArrayList<>();
        CollectRequest collect = new CollectRequest();
        for (Dependency dependency : kotlin.getDependencies()) {
            collect.addDependency(new org.eclipse.aether.graph.Dependency(new DefaultArtifact(dependency.getGroupId(),
                    dependency.getArtifactId(), dependency.getClassifier(),
                    dependency.getType() == null ? "jar" : dependency.getType(), dependency.getVersion()), "runtime"));
        }
        collect.setRepositories(project.getRemotePluginRepositories());
        return resolve(collect, "kotlin-maven-plugin's dependencies");
    }

    private List<File> resolve(CollectRequest collect, String what) throws MojoExecutionException {
        List<File> jars = new ArrayList<>();
        try {
            for (ArtifactResult result : repositorySystem.resolveDependencies(session.getRepositorySession(),
                    new DependencyRequest(collect, null)).getArtifactResults()) {
                jars.add(result.getArtifact().getFile());
            }
        } catch (DependencyResolutionException e) {
            throw new MojoExecutionException("cannot resolve " + what, e);
        }
        return jars;
    }

    private File krusty() throws MojoExecutionException {
        if (binary != null) return binary;
        String environment = System.getenv("KRUSTY_BIN");
        if (environment != null && !environment.isEmpty()) return new File(environment);
        String release = KrustyRelease.pluginRelease();
        if (release == null) {
            throw new MojoExecutionException("this krusty-maven-plugin build names no krusty release; set -Dkrusty.binary=<path>");
        }
        File cache = new File(session.getRepositorySession().getLocalRepository().getBasedir(),
                "dev/krustythecompiler/krusty-compiler");
        return KrustyRelease.compiler(release, releases, cache, session.isOffline());
    }

    /** Run krusty, logging what it prints; a failure here is krusty's own, never the compilation's. */
    private void run(List<String> command) throws MojoExecutionException {
        try {
            Process process = new ProcessBuilder(command).redirectErrorStream(true).start();
            try (BufferedReader output = new BufferedReader(
                    new InputStreamReader(process.getInputStream(), StandardCharsets.UTF_8))) {
                for (String line; (line = output.readLine()) != null; ) getLog().info(line);
            }
            int status = process.waitFor();
            if (status != 0) throw new MojoExecutionException("krusty " + command.get(1) + " exited with " + status);
        } catch (IOException e) {
            throw new MojoExecutionException("cannot run " + command.get(0), e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new MojoExecutionException("interrupted while krusty ran", e);
        }
    }

    private static File temporary(String text) throws MojoExecutionException {
        try {
            File file = Files.createTempFile("krusty-verify-", ".unsupported").toFile();
            Files.writeString(file.toPath(), text, StandardCharsets.UTF_8);
            return file;
        } catch (IOException e) {
            throw new MojoExecutionException("cannot write the unsupported reasons", e);
        }
    }
}
