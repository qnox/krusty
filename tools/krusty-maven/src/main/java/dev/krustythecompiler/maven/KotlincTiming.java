package dev.krustythecompiler.maven;

import javax.inject.Named;
import javax.inject.Singleton;
import org.apache.maven.AbstractMavenLifecycleParticipant;
import org.apache.maven.execution.MavenSession;
import org.apache.maven.execution.MojoExecutionEvent;
import org.apache.maven.execution.MojoExecutionListener;
import org.apache.maven.plugin.MojoExecution;
import org.apache.maven.project.MavenProject;

/**
 * Times kotlin-maven-plugin's {@code compile} and {@code test-compile} as the build runs them, from
 * the start to the end of the mojo's own execution, and leaves each time on the project for the
 * verify goal: Maven tells a goal nothing of how long an earlier mojo took. The listener sees those
 * mojos only when the plugin is loaded before them, as a build extension: declared with
 * {@code <extensions>true</extensions>} or listed in {@code .mvn/extensions.xml}.
 */
@Named
@Singleton
public class KotlincTiming implements MojoExecutionListener {
    private static final String STARTED = "krusty.kotlinc.started:";
    private static final String MILLIS = "krusty.kotlinc.millis:";
    /** Set on every project when the plugin is a build extension, so the goal can say why a time is missing. */
    static final String LOADED = "krusty.kotlinc.timing";

    /** The project context key of the time of kotlin-maven-plugin's {@code goal} in {@code execution}. */
    static String key(String goal, String execution) {
        return MILLIS + goal + "@" + execution;
    }

    @Override
    public void beforeMojoExecution(MojoExecutionEvent event) {
        if (timed(event.getExecution())) {
            event.getProject().setContextValue(started(event.getExecution()), System.nanoTime());
        }
    }

    @Override
    public void afterMojoExecutionSuccess(MojoExecutionEvent event) {
        MojoExecution execution = event.getExecution();
        if (!timed(execution)) return;
        MavenProject project = event.getProject();
        Object started = project.getContextValue(started(execution));
        if (started instanceof Long) {
            long millis = (System.nanoTime() - (Long) started) / 1_000_000;
            project.setContextValue(key(execution.getGoal(), execution.getExecutionId()), millis);
        }
    }

    @Override
    public void afterExecutionFailure(MojoExecutionEvent event) {
    }

    private static boolean timed(MojoExecution execution) {
        return KotlinCompilation.KOTLIN_PLUGIN.equals(execution.getGroupId() + ":" + execution.getArtifactId())
                && (execution.getGoal().equals("compile") || execution.getGoal().equals("test-compile"));
    }

    private static String started(MojoExecution execution) {
        return STARTED + execution.getGoal() + "@" + execution.getExecutionId();
    }

    /**
     * Marks every project as timed. Maven calls a lifecycle participant only from a build extension,
     * before any mojo runs; the plugin's own realm, loaded for the goal, also registers the listener,
     * but too late to see the compile.
     */
    @Named
    @Singleton
    public static class Extension extends AbstractMavenLifecycleParticipant {
        @Override
        public void afterProjectsRead(MavenSession session) {
            for (MavenProject project : session.getProjects()) project.setContextValue(LOADED, Boolean.TRUE);
        }
    }
}
