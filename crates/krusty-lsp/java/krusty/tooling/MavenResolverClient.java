package krusty.tooling;

import java.io.BufferedReader;
import java.io.File;
import java.io.InputStreamReader;
import java.net.URISyntaxException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import org.apache.maven.RepositoryUtils;
import org.apache.maven.artifact.repository.ArtifactRepository;
import org.apache.maven.bridge.MavenRepositorySystem;
import org.apache.maven.cli.MavenCli;
import org.apache.maven.cli.configuration.SettingsXmlConfigurationProcessor;
import org.apache.maven.execution.DefaultMavenExecutionRequest;
import org.apache.maven.execution.MavenExecutionRequest;
import org.apache.maven.execution.MavenExecutionRequestPopulator;
import org.apache.maven.internal.aether.DefaultRepositorySystemSessionFactory;
import org.apache.maven.model.Repository;
import org.apache.maven.model.RepositoryPolicy;
import org.apache.maven.settings.Settings;
import org.apache.maven.settings.building.SettingsBuilder;
import org.apache.maven.settings.building.SettingsBuildingRequest;
import org.apache.maven.settings.building.DefaultSettingsBuildingRequest;
import org.codehaus.plexus.DefaultContainerConfiguration;
import org.codehaus.plexus.DefaultPlexusContainer;
import org.eclipse.aether.RepositorySystem;
import org.eclipse.aether.RepositorySystemSession;
import org.eclipse.aether.artifact.Artifact;
import org.eclipse.aether.artifact.DefaultArtifact;
import org.eclipse.aether.collection.CollectRequest;
import org.eclipse.aether.graph.Dependency;
import org.eclipse.aether.graph.Exclusion;
import org.eclipse.aether.repository.RemoteRepository;
import org.eclipse.aether.resolution.ArtifactRequest;
import org.eclipse.aether.resolution.DependencyRequest;

/**
 * Long-lived Maven resolver. The session is the one
 * {@link DefaultRepositorySystemSessionFactory} builds from {@code settings.xml}: mirrors,
 * proxies, servers, and the local repository. A repository from {@code jarRepositories.xml}
 * is added to that request and normalized by the session. Artifact requests go through
 * {@link RepositorySystem#resolveArtifact} and {@link RepositorySystem#resolveDependencies},
 * the calls IntelliJ's embedder makes from {@code Maven3XServerEmbedder.resolveArtifacts}.
 */
public final class MavenResolverClient {
    private static MavenRuntime runtime;

    static {
        if (System.getProperty("maven.conf") == null) {
            File home = mavenHome();
            if (home != null) {
                File conf = new File(home, "conf");
                if (conf.isDirectory()) {
                    System.setProperty("maven.conf", conf.getAbsolutePath());
                }
            }
        }
    }

    private MavenResolverClient() {}

    public static void main(String[] args) throws Exception {
        BufferedReader input = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
        String line;
        while ((line = input.readLine()) != null) {
            if (line.isEmpty()) {
                continue;
            }
            try {
                resolve(Protocol.object(line));
            } catch (Throwable failure) {
                Protocol.respond(1, "", failure.toString());
            }
        }
        close();
    }

    private static void resolve(Map<String, Object> request) throws Exception {
        ensureRuntime();
        List<RemoteRepository> remotes = withProjectRepositories(runtime, Protocol.objects(request, "repositories"));
        StringBuilder errors = new StringBuilder();
        for (Map<String, Object> artifact : Protocol.objects(request, "artifacts")) {
            try {
                resolveOne(artifact, remotes);
            } catch (Exception failure) {
                if (!errors.isEmpty()) {
                    errors.append('\n');
                }
                errors.append(failure);
            }
        }
        if (errors.isEmpty()) {
            Protocol.respond(0, "", "");
        } else {
            Protocol.respond(1, "", errors.toString());
        }
    }

    private static void resolveOne(Map<String, Object> requested, List<RemoteRepository> remotes)
        throws Exception {
        String classifier = Protocol.text(requested, "classifier");
        if (classifier.isEmpty()) {
            classifier = null;
        }
        String extension = Protocol.text(requested, "extension");
        if (extension.isEmpty()) {
            extension = "jar";
        }
        Artifact artifact = new DefaultArtifact(
            Protocol.text(requested, "group"),
            Protocol.text(requested, "artifact"),
            classifier,
            extension,
            Protocol.text(requested, "version")
        );
        if (Protocol.flag(requested, "transitive")) {
            List<Exclusion> exclusions = new ArrayList<>();
            for (Map<String, Object> exclusion : Protocol.objects(requested, "exclusions")) {
                exclusions.add(new Exclusion(
                    Protocol.text(exclusion, "group"),
                    Protocol.text(exclusion, "artifact"),
                    "*",
                    "*"
                ));
            }
            CollectRequest collect = new CollectRequest();
            collect.setRoot(new Dependency(artifact, "compile", false, exclusions));
            collect.setRepositories(remotes);
            runtime.system.resolveDependencies(runtime.session, new DependencyRequest(collect, null));
            return;
        }
        runtime.system.resolveArtifact(runtime.session, new ArtifactRequest(artifact, remotes, null));
    }

    private static void ensureRuntime() throws Exception {
        if (runtime == null) {
            runtime = open(
                SettingsXmlConfigurationProcessor.DEFAULT_USER_SETTINGS_FILE, globalSettings()
            );
        }
    }

    private static void close() {
        if (runtime != null) {
            runtime.close();
            runtime = null;
        }
    }

    /**
     * The embedder session for one settings pair. {@code globalSettings} may be absent.
     * Project repositories are not part of this session until
     * {@link #withProjectRepositories} adds them.
     */
    static MavenRuntime open(File userSettings, File globalSettings) throws Exception {
        DefaultContainerConfiguration configuration = new DefaultContainerConfiguration();
        configuration.setClassPathScanning("index");
        configuration.setAutoWiring(true);
        configuration.setJSR250Lifecycle(true);
        configuration.setName("maven");
        DefaultPlexusContainer container = new DefaultPlexusContainer(configuration);
        try {
            Thread.currentThread().setContextClassLoader(container.getContainerRealm());
            DefaultRepositorySystemSessionFactory factory =
                container.lookup(DefaultRepositorySystemSessionFactory.class);
            MavenExecutionRequestPopulator populator = container.lookup(MavenExecutionRequestPopulator.class);
            RepositorySystem system = container.lookup(RepositorySystem.class);
            SettingsBuilder builder = container.lookup(SettingsBuilder.class);
            Settings settings = settings(builder, userSettings, globalSettings);
            MavenExecutionRequest request = new DefaultMavenExecutionRequest();
            populator.populateFromSettings(request, settings);
            populator.populateDefaults(request);
            RepositorySystemSession session = factory.newRepositorySession(request);
            List<RemoteRepository> repositories = system.newResolutionRepositories(
                session, RepositoryUtils.toRepos(request.getRemoteRepositories())
            );
            return new MavenRuntime(container, system, session, repositories);
        } catch (Exception failure) {
            container.dispose();
            throw failure;
        }
    }

    /**
     * Repositories named by {@code jarRepositories.xml}, turned into Maven repositories and
     * normalized by {@code runtime}'s session. A URL the session already has is left as that
     * resolved entry. Their update policy is always: a {@code .lastUpdated} marker left by a
     * failed Central lookup must not hide a JetBrains-only coordinate.
     */
    static List<RemoteRepository> withProjectRepositories(
        MavenRuntime runtime, List<? extends Map<String, ?>> requested
    ) throws Exception {
        List<ArtifactRepository> added = new ArrayList<>();
        for (Map<String, ?> repository : requested) {
            String url = text(repository, "url");
            if (url.isEmpty() || listed(runtime.repositories, url) || listedArtifact(added, url)) {
                continue;
            }
            String id = text(repository, "id");
            if (id.isEmpty()) {
                id = url;
            }
            added.add(MavenRepositorySystem.buildArtifactRepository(projectRepository(id, url)));
        }
        if (added.isEmpty()) {
            return runtime.repositories;
        }
        List<RemoteRepository> normalized =
            runtime.system.newResolutionRepositories(runtime.session, RepositoryUtils.toRepos(added));
        List<RemoteRepository> remotes = new ArrayList<>(runtime.repositories);
        remotes.addAll(normalized);
        return remotes;
    }

    private static Repository projectRepository(String id, String url) {
        Repository repository = new Repository();
        repository.setId(id);
        repository.setUrl(url);
        repository.setLayout("default");
        repository.setReleases(always());
        repository.setSnapshots(always());
        return repository;
    }

    private static RepositoryPolicy always() {
        RepositoryPolicy policy = new RepositoryPolicy();
        policy.setEnabled(true);
        policy.setUpdatePolicy(org.eclipse.aether.repository.RepositoryPolicy.UPDATE_POLICY_ALWAYS);
        policy.setChecksumPolicy(org.eclipse.aether.repository.RepositoryPolicy.CHECKSUM_POLICY_WARN);
        return policy;
    }

    private static Settings settings(SettingsBuilder builder, File userSettings, File globalSettings)
        throws Exception {
        SettingsBuildingRequest building = new DefaultSettingsBuildingRequest();
        building.setSystemProperties(System.getProperties());
        if (userSettings != null && userSettings.isFile()) {
            building.setUserSettingsFile(userSettings);
        }
        if (globalSettings != null && globalSettings.isFile()) {
            building.setGlobalSettingsFile(globalSettings);
        }
        return builder.build(building).getEffectiveSettings();
    }

    private static File globalSettings() {
        File configured = SettingsXmlConfigurationProcessor.DEFAULT_GLOBAL_SETTINGS_FILE;
        if (configured.isFile()) {
            return configured;
        }
        File home = mavenHome();
        if (home == null) {
            return configured;
        }
        return new File(new File(home, "conf"), "settings.xml");
    }

    private static File mavenHome() {
        String property = System.getProperty("maven.home");
        if (property != null && !property.isEmpty()) {
            return new File(property);
        }
        try {
            File jar = new File(MavenCli.class.getProtectionDomain().getCodeSource().getLocation().toURI());
            File lib = jar.getParentFile();
            if (lib != null && "lib".equals(lib.getName())) {
                return lib.getParentFile();
            }
        } catch (URISyntaxException ignored) {
            return null;
        }
        return null;
    }

    private static String text(Map<String, ?> object, String key) {
        Object value = object.get(key);
        return value == null ? "" : value.toString();
    }

    private static boolean listed(List<RemoteRepository> repositories, String url) {
        for (RemoteRepository repository : repositories) {
            if (url.equals(repository.getUrl())) {
                return true;
            }
        }
        return false;
    }

    private static boolean listedArtifact(List<ArtifactRepository> repositories, String url) {
        for (ArtifactRepository repository : repositories) {
            if (url.equals(repository.getUrl())) {
                return true;
            }
        }
        return false;
    }

    static final class MavenRuntime {
        final RepositorySystem system;
        final RepositorySystemSession session;
        final List<RemoteRepository> repositories;
        private final DefaultPlexusContainer container;

        private MavenRuntime(
            DefaultPlexusContainer container,
            RepositorySystem system,
            RepositorySystemSession session,
            List<RemoteRepository> repositories
        ) {
            this.container = container;
            this.system = system;
            this.session = session;
            this.repositories = repositories;
        }

        void close() {
            try {
                container.dispose();
            } catch (Exception ignored) {
                // The process is leaving. A container that fails to stop has nothing left to do.
            }
        }
    }
}
