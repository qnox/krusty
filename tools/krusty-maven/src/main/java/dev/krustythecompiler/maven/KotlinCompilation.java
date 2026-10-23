package dev.krustythecompiler.maven;

import java.io.File;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import org.apache.maven.model.Plugin;
import org.apache.maven.model.PluginExecution;
import org.apache.maven.project.MavenProject;
import org.codehaus.plexus.util.xml.Xpp3Dom;

/**
 * One kotlin-maven-plugin compilation ({@code compile} or {@code test-compile}) of a project, as the
 * kotlinc request krusty receives: kotlin-maven-plugin's own configuration, read from the project
 * model the way that plugin reads it, plus the sources and classpath Maven resolved.
 */
final class KotlinCompilation {
    static final String KOTLIN_PLUGIN = "org.jetbrains.kotlin:kotlin-maven-plugin";

    /** kotlin-maven-plugin's compiler plugin names, by the id of the kotlinc plugin each selects. */
    private static final Map<String, String> PLUGIN_IDS = Map.of(
            "all-open", "org.jetbrains.kotlin.allopen",
            "spring", "org.jetbrains.kotlin.allopen",
            "no-arg", "org.jetbrains.kotlin.noarg",
            "jpa", "org.jetbrains.kotlin.noarg",
            "sam-with-receiver", "org.jetbrains.kotlin.samWithReceiver",
            "kotlinx-serialization", "org.jetbrains.kotlinx.serialization");

    final String name;
    final String sourceSet;
    /** Where krusty writes, under the project's {@code krusty-verify} directory. */
    final String outputName;
    final File reference;
    final List<File> sources;
    /** kotlinc's arguments other than the sources and {@code -d}. */
    final List<String> arguments;
    /** Why krusty cannot be given this request, when the plugin cannot derive it. */
    final List<String> unsupported;

    private KotlinCompilation(String name, String sourceSet, String outputName, File reference,
            List<File> sources, List<String> arguments, List<String> unsupported) {
        this.name = name;
        this.sourceSet = sourceSet;
        this.outputName = outputName;
        this.reference = reference;
        this.sources = sources;
        this.arguments = arguments;
        this.unsupported = unsupported;
    }

    /**
     * The project's compilations that kotlin-maven-plugin's {@code goal} runs, one per execution. Two
     * executions of one goal write into one output directory, where kotlinc's output of each cannot
     * be told apart, so each is then unsupported rather than compared against the other's classes.
     */
    static List<KotlinCompilation> of(MavenProject project, Plugin kotlin, String goal, List<String> classpath,
            List<File> pluginClasspath, String jdkHome) {
        List<PluginExecution> executions = new ArrayList<>();
        for (PluginExecution execution : kotlin.getExecutions()) {
            if (execution.getGoals().contains(goal)) executions.add(execution);
        }
        List<KotlinCompilation> compilations = new ArrayList<>();
        boolean shared = executions.size() > 1;
        for (PluginExecution execution : executions) {
            compilations.add(of(project, kotlin, goal, execution, shared ? executions.size() : 0, classpath,
                    pluginClasspath, jdkHome));
        }
        return compilations;
    }

    private static KotlinCompilation of(MavenProject project, Plugin kotlin, String goal, PluginExecution execution,
            int sharing, List<String> classpath, List<File> pluginClasspath, String jdkHome) {
        Xpp3Dom configuration = merged(execution.getConfiguration(), kotlin.getConfiguration());
        boolean test = goal.equals("test-compile");
        File output = new File(test ? project.getBuild().getTestOutputDirectory() : project.getBuild().getOutputDirectory());

        Set<File> roots = new LinkedHashSet<>();
        List<String> sourceDirs = values(configuration, "sourceDirs");
        if (sourceDirs.isEmpty()) {
            for (String root : test ? project.getTestCompileSourceRoots() : project.getCompileSourceRoots()) {
                roots.add(new File(root));
            }
            roots.add(new File(project.getBasedir(), test ? "src/test/kotlin" : "src/main/kotlin"));
        } else {
            for (String root : sourceDirs) roots.add(absolute(project, root));
        }
        List<File> sources = new ArrayList<>();
        for (File root : roots) collect(root, sources);
        sources.sort(null);

        List<String> unsupported = new ArrayList<>();
        if (sharing > 0) {
            unsupported.add("krusty: kotlin-maven-plugin runs " + sharing + " executions of '" + goal
                    + "' into one output directory, so kotlinc's output of each cannot be told apart");
        }
        List<String> arguments = new ArrayList<>(values(configuration, "args"));
        List<String> libraries = new ArrayList<>();
        for (String element : classpath) {
            if (!new File(element).equals(output)) libraries.add(element);
        }
        if (!libraries.isEmpty()) {
            arguments.add("-classpath");
            arguments.add(String.join(File.pathSeparator, libraries));
        }
        if (test) arguments.add("-Xfriend-paths=" + new File(project.getBuild().getOutputDirectory()).getAbsolutePath());
        String moduleName = value(configuration, "moduleName", project, "kotlin.compiler.moduleName");
        // kotlin-maven-plugin names both compilations' module after the artifact (2.4.20).
        if (moduleName == null) moduleName = project.getArtifactId();
        addPair(arguments, "-module-name", moduleName);
        addPair(arguments, "-jvm-target", value(configuration, "jvmTarget", project, "kotlin.compiler.jvmTarget"));
        addPair(arguments, "-language-version",
                value(configuration, "languageVersion", project, "kotlin.compiler.languageVersion"));
        addPair(arguments, "-api-version", value(configuration, "apiVersion", project, "kotlin.compiler.apiVersion"));
        if ("true".equals(value(configuration, "javaParameters", project, null))) arguments.add("-java-parameters");
        if ("true".equals(value(configuration, "nowarn", project, "kotlin.compiler.suppressWarnings"))) {
            arguments.add("-nowarn");
        }
        String jdk = value(configuration, "jdkHome", project, "kotlin.compiler.jdkHome");
        addPair(arguments, "-jdk-home", jdk != null ? jdk : jdkHome);

        if (!pluginClasspath.isEmpty()) {
            List<String> jars = new ArrayList<>();
            for (File jar : pluginClasspath) jars.add(jar.getAbsolutePath());
            arguments.add("-Xplugin=" + String.join(",", jars));
        }
        for (String plugin : values(configuration, "compilerPlugins")) {
            if (!PLUGIN_IDS.containsKey(plugin)) {
                unsupported.add("krusty: kotlin-maven-plugin compiler plugin '" + plugin + "' is not supported");
            } else if (plugin.equals("spring") || plugin.equals("jpa")) {
                arguments.add("-P");
                arguments.add("plugin:" + PLUGIN_IDS.get(plugin) + ":preset=" + plugin);
            }
        }
        for (String option : values(configuration, "pluginOptions")) {
            int colon = option.indexOf(':');
            String id = colon < 0 ? null : PLUGIN_IDS.get(option.substring(0, colon));
            if (id == null) {
                unsupported.add("krusty: kotlin-maven-plugin plugin option '" + option + "' is not supported");
            } else {
                arguments.add("-P");
                arguments.add("plugin:" + id + ":" + option.substring(colon + 1));
            }
        }
        arguments.add("-Xkotlin-reference-version=" + kotlin.getVersion());
        arguments.add("-no-stdlib");
        arguments.add("-no-reflect");
        String sourceSet = test ? "test" : "main";
        String name = project.getArtifactId() + ":" + goal + (sharing > 0 ? "@" + execution.getId() : "");
        String outputName = sharing > 0 ? sourceSet + "-" + execution.getId() : sourceSet;
        return new KotlinCompilation(name, sourceSet, outputName, output, sources, arguments, unsupported);
    }

    private static Xpp3Dom merged(Object execution, Object plugin) {
        Xpp3Dom dominant = execution instanceof Xpp3Dom ? new Xpp3Dom((Xpp3Dom) execution) : null;
        Xpp3Dom recessive = plugin instanceof Xpp3Dom ? (Xpp3Dom) plugin : null;
        if (dominant == null) return recessive == null ? new Xpp3Dom("configuration") : recessive;
        return Xpp3Dom.mergeXpp3Dom(dominant, recessive);
    }

    /** A parameter's value: the configuration's, else the user property kotlin-maven-plugin reads. */
    private static String value(Xpp3Dom configuration, String name, MavenProject project, String property) {
        Xpp3Dom child = configuration.getChild(name);
        if (child != null && child.getValue() != null && !child.getValue().isBlank()) return child.getValue().trim();
        return property == null ? null : project.getProperties().getProperty(property);
    }

    private static List<String> values(Xpp3Dom configuration, String name) {
        List<String> found = new ArrayList<>();
        Xpp3Dom list = configuration.getChild(name);
        if (list == null) return found;
        for (Xpp3Dom item : list.getChildren()) {
            if (item.getValue() != null && !item.getValue().isBlank()) found.add(item.getValue().trim());
        }
        return found;
    }

    private static File absolute(MavenProject project, String path) {
        File file = new File(path);
        return file.isAbsolute() ? file : new File(project.getBasedir(), path);
    }

    private static void collect(File directory, List<File> sources) {
        File[] children = directory.listFiles();
        if (children == null) return;
        for (File child : children) {
            if (child.isDirectory()) {
                collect(child, sources);
            } else if (child.getName().endsWith(".kt") || child.getName().endsWith(".java")) {
                sources.add(child.getAbsoluteFile());
            }
        }
    }

    private static void addPair(List<String> arguments, String name, String value) {
        if (value != null) {
            arguments.add(name);
            arguments.add(value);
        }
    }

    boolean hasKotlinSources() {
        return sources.stream().anyMatch(source -> source.getName().endsWith(".kt"));
    }
}
