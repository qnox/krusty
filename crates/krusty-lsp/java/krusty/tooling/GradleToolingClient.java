package krusty.tooling;

import java.io.BufferedReader;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.gradle.tooling.BuildException;
import org.gradle.tooling.BuildLauncher;
import org.gradle.tooling.GradleConnectionException;
import org.gradle.tooling.GradleConnector;
import org.gradle.tooling.ProjectConnection;

/**
 * Long-lived Gradle Tooling API client. One process holds a {@link ProjectConnection} per project
 * directory and runs build actions on the project's daemon. The distribution comes from the
 * project's wrapper via {@code useBuildDistribution()}.
 */
public final class GradleToolingClient {
    private static final Map<String, ProjectConnection> CONNECTIONS = new LinkedHashMap<>();

    private GradleToolingClient() {}

    public static void main(String[] args) throws Exception {
        Protocol.capture();
        Runtime.getRuntime().addShutdownHook(new Thread(GradleToolingClient::closeAll));
        BufferedReader input = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
        String line;
        while ((line = input.readLine()) != null) {
            if (line.isEmpty()) {
                continue;
            }
            try {
                run(Protocol.object(line));
            } catch (Throwable failure) {
                Protocol.respond(1, "", failure.toString());
            }
        }
        closeAll();
    }

    private static void run(Map<String, Object> request) {
        File projectDir = new File(Protocol.text(request, "projectDir"));
        List<String> tasks = Protocol.strings(request, "tasks");
        List<String> arguments = Protocol.strings(request, "arguments");
        ByteArrayOutputStream stdout = new ByteArrayOutputStream();
        ByteArrayOutputStream stderr = new ByteArrayOutputStream();
        try {
            // Same entry IntelliJ uses: GradleConnector.newConnector(), then a ProjectConnection
            // whose build action runs on the long-lived daemon located by the wrapper.
            ProjectConnection connection = connection(projectDir);
            BuildLauncher build = connection
                .newBuild()
                .forTasks(tasks.toArray(String[]::new))
                .withArguments(arguments.toArray(String[]::new))
                .setStandardOutput(stdout)
                .setStandardError(stderr);
            // The client JVM may be newer than the project's Gradle. This is the daemon JVM for
            // this build, so the init script is compiled by a JVM that Gradle can read.
            String javaHome = Protocol.text(request, "javaHome");
            if (!javaHome.isEmpty()) {
                build.setJavaHome(new File(javaHome));
            }
            build.run();
            Protocol.respond(0, stdout.toString(StandardCharsets.UTF_8), stderr.toString(StandardCharsets.UTF_8));
        } catch (BuildException failure) {
            Protocol.respond(1, stdout.toString(StandardCharsets.UTF_8), detail(stderr, failure));
        } catch (GradleConnectionException failure) {
            close(projectDir);
            Protocol.respond(1, stdout.toString(StandardCharsets.UTF_8), detail(stderr, failure));
        }
    }

    private static ProjectConnection connection(File projectDir) {
        String key = projectDir.getAbsolutePath();
        ProjectConnection existing = CONNECTIONS.get(key);
        if (existing != null) {
            return existing;
        }
        ProjectConnection created = GradleConnector.newConnector()
            .forProjectDirectory(projectDir)
            .useBuildDistribution()
            .connect();
        CONNECTIONS.put(key, created);
        return created;
    }

    private static String detail(ByteArrayOutputStream stderr, Exception failure) {
        String text = stderr.toString(StandardCharsets.UTF_8);
        if (!text.isEmpty() && !text.endsWith("\n")) {
            text += "\n";
        }
        return text + failure;
    }

    private static void close(File projectDir) {
        ProjectConnection connection = CONNECTIONS.remove(projectDir.getAbsolutePath());
        if (connection != null) {
            connection.close();
        }
    }

    private static void closeAll() {
        for (ProjectConnection connection : CONNECTIONS.values()) {
            connection.close();
        }
        CONNECTIONS.clear();
    }
}
