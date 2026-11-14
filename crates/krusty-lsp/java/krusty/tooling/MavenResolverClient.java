package krusty.tooling;

import java.io.BufferedReader;
import java.io.File;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import org.apache.maven.repository.internal.MavenRepositorySystemUtils;
import org.apache.maven.settings.Mirror;
import org.apache.maven.settings.Profile;
import org.apache.maven.settings.Proxy;
import org.apache.maven.settings.Repository;
import org.apache.maven.settings.Server;
import org.apache.maven.settings.Settings;
import org.apache.maven.settings.building.DefaultSettingsBuilderFactory;
import org.apache.maven.settings.building.DefaultSettingsBuildingRequest;
import org.apache.maven.wagon.Wagon;
import org.apache.maven.wagon.providers.file.FileWagon;
import org.apache.maven.wagon.providers.http.HttpWagon;
import org.eclipse.aether.DefaultRepositorySystemSession;
import org.eclipse.aether.RepositorySystem;
import org.eclipse.aether.RepositorySystemSession;
import org.eclipse.aether.artifact.Artifact;
import org.eclipse.aether.artifact.DefaultArtifact;
import org.eclipse.aether.collection.CollectRequest;
import org.eclipse.aether.graph.Dependency;
import org.eclipse.aether.graph.Exclusion;
import org.eclipse.aether.impl.DefaultServiceLocator;
import org.eclipse.aether.repository.LocalRepository;
import org.eclipse.aether.repository.RemoteRepository;
import org.eclipse.aether.repository.RepositoryPolicy;
import org.eclipse.aether.resolution.ArtifactRequest;
import org.eclipse.aether.resolution.DependencyRequest;
import org.eclipse.aether.connector.basic.BasicRepositoryConnectorFactory;
import org.eclipse.aether.spi.connector.RepositoryConnectorFactory;
import org.eclipse.aether.spi.connector.transport.TransporterFactory;
import org.eclipse.aether.transport.wagon.WagonProvider;
import org.eclipse.aether.transport.wagon.WagonTransporterFactory;
import org.eclipse.aether.util.repository.AuthenticationBuilder;
import org.eclipse.aether.util.repository.DefaultAuthenticationSelector;
import org.eclipse.aether.util.repository.DefaultMirrorSelector;
import org.eclipse.aether.util.repository.DefaultProxySelector;

/**
 * Long-lived Maven resolver. Artifact requests go through
 * {@link RepositorySystem#resolveArtifact} and {@link RepositorySystem#resolveDependencies}, the
 * calls IntelliJ's embedder makes from {@code Maven3XServerEmbedder.resolveArtifacts}.
 */
public final class MavenResolverClient {
    private static RepositorySystem repositorySystem;
    private static RepositorySystemSession session;
    private static List<RemoteRepository> repositories;

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
    }

    private static void resolve(Map<String, Object> request) throws Exception {
        ensureSession();
        List<RemoteRepository> remotes = repositoriesFor(request);
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
            repositorySystem.resolveDependencies(session, new DependencyRequest(collect, null));
            return;
        }
        repositorySystem.resolveArtifact(session, new ArtifactRequest(artifact, remotes, null));
    }

    /**
     * Project repositories from {@code jarRepositories.xml} join the session repositories
     * the way IntelliJ's embedder does. Their update policy is always: a {@code .lastUpdated}
     * marker left by a failed Central lookup must not hide a JetBrains-only coordinate.
     * Each new repository passes through {@link RepositorySystem#newResolutionRepositories}
     * so the session's mirror, proxy, and server credentials apply before a request uses it.
     */
    private static List<RemoteRepository> repositoriesFor(Map<String, Object> request) {
        return resolutionRepositories(
            repositorySystem, session, repositories, projectRepositories(request)
        );
    }

    private static List<RemoteRepository> projectRepositories(Map<String, Object> request) {
        RepositoryPolicy always = new RepositoryPolicy(
            true, RepositoryPolicy.UPDATE_POLICY_ALWAYS, RepositoryPolicy.CHECKSUM_POLICY_WARN
        );
        List<RemoteRepository> requested = new ArrayList<>();
        for (Map<String, Object> repository : Protocol.objects(request, "repositories")) {
            String url = Protocol.text(repository, "url");
            if (url.isEmpty()) {
                continue;
            }
            String id = Protocol.text(repository, "id");
            if (id.isEmpty()) {
                id = url;
            }
            requested.add(repository(id, url, always));
        }
        return requested;
    }

    /**
     * Normalize {@code projectRepositories} on {@code session} and append them to the
     * repositories the session already resolved. A URL the session already has is left
     * as that resolved entry.
     */
    static List<RemoteRepository> resolutionRepositories(
        RepositorySystem system,
        RepositorySystemSession session,
        List<RemoteRepository> sessionRepositories,
        List<RemoteRepository> projectRepositories
    ) {
        List<RemoteRepository> added = new ArrayList<>();
        for (RemoteRepository repository : projectRepositories) {
            String url = repository.getUrl();
            if (url == null
                || url.isEmpty()
                || listed(sessionRepositories, url)
                || listed(added, url)) {
                continue;
            }
            added.add(repository);
        }
        if (added.isEmpty()) {
            return sessionRepositories;
        }
        List<RemoteRepository> normalized = system.newResolutionRepositories(session, added);
        List<RemoteRepository> remotes = new ArrayList<>(sessionRepositories);
        remotes.addAll(normalized);
        return remotes;
    }

    private static boolean listed(List<RemoteRepository> repositories, String url) {
        for (RemoteRepository repository : repositories) {
            if (url.equals(repository.getUrl())) {
                return true;
            }
        }
        return false;
    }

    private static void ensureSession() throws Exception {
        if (session != null) {
            return;
        }
        DefaultServiceLocator locator = MavenRepositorySystemUtils.newServiceLocator();
        locator.addService(RepositoryConnectorFactory.class, BasicRepositoryConnectorFactory.class);
        locator.addService(TransporterFactory.class, WagonTransporterFactory.class);
        locator.setServices(WagonProvider.class, new ManualWagonProvider());
        repositorySystem = locator.getService(RepositorySystem.class);
        if (repositorySystem == null) {
            throw new IllegalStateException("RepositorySystem is unavailable");
        }
        Settings settings = settings();
        DefaultRepositorySystemSession created = MavenRepositorySystemUtils.newSession();
        created.setOffline(settings.isOffline());
        String local = settings.getLocalRepository();
        if (local == null || local.isEmpty()) {
            local = new File(System.getProperty("user.home"), ".m2/repository").getAbsolutePath();
        }
        created.setLocalRepositoryManager(
            repositorySystem.newLocalRepositoryManager(created, new LocalRepository(local))
        );
        created.setMirrorSelector(mirrorSelector(settings));
        DefaultAuthenticationSelector authentication = new DefaultAuthenticationSelector();
        for (Server server : settings.getServers()) {
            if (server.getUsername() == null) {
                continue;
            }
            authentication.add(
                server.getId(),
                new AuthenticationBuilder()
                    .addUsername(server.getUsername())
                    .addPassword(server.getPassword())
                    .build()
            );
        }
        created.setAuthenticationSelector(authentication);
        DefaultProxySelector proxies = new DefaultProxySelector();
        for (Proxy proxy : settings.getProxies()) {
            if (!proxy.isActive() || proxy.getHost() == null) {
                continue;
            }
            org.eclipse.aether.repository.Authentication proxyAuthentication = null;
            if (proxy.getUsername() != null) {
                proxyAuthentication = new AuthenticationBuilder()
                    .addUsername(proxy.getUsername())
                    .addPassword(proxy.getPassword())
                    .build();
            }
            proxies.add(
                new org.eclipse.aether.repository.Proxy(
                    proxy.getProtocol(), proxy.getHost(), proxy.getPort(), proxyAuthentication
                ),
                proxy.getNonProxyHosts()
            );
        }
        created.setProxySelector(proxies);
        repositories = repositorySystem.newResolutionRepositories(created, remoteRepositories(settings));
        session = created;
    }

    /**
     * {@code add(id, url, type, repositoryManager, blocked, mirrorOfIds, mirrorOfTypes)}.
     * Maven's {@code mirrorOf} is the repository-id pattern, not the content type.
     */
    static DefaultMirrorSelector mirrorSelector(Settings settings) {
        DefaultMirrorSelector mirrors = new DefaultMirrorSelector();
        for (Mirror mirror : settings.getMirrors()) {
            String mirrorOf = mirror.getMirrorOf();
            if (mirrorOf == null
                || mirrorOf.isEmpty()
                || mirror.getUrl() == null
                || mirror.getUrl().isEmpty()) {
                continue;
            }
            mirrors.add(
                mirror.getId(),
                mirror.getUrl(),
                null,
                false,
                false,
                mirrorOf,
                mirror.getMirrorOfLayouts()
            );
        }
        return mirrors;
    }

    private static Settings settings() {
        try {
            DefaultSettingsBuildingRequest request = new DefaultSettingsBuildingRequest();
            request.setSystemProperties(System.getProperties());
            File user = new File(System.getProperty("user.home"), ".m2/settings.xml");
            if (user.isFile()) {
                request.setUserSettingsFile(user);
            }
            File global = new File("/usr/share/maven/conf/settings.xml");
            if (global.isFile()) {
                request.setGlobalSettingsFile(global);
            }
            return new DefaultSettingsBuilderFactory().newInstance().build(request).getEffectiveSettings();
        } catch (Exception ignored) {
            return new Settings();
        }
    }

    private static List<RemoteRepository> remoteRepositories(Settings settings) {
        RepositoryPolicy policy = new RepositoryPolicy(
            true, RepositoryPolicy.UPDATE_POLICY_DAILY, RepositoryPolicy.CHECKSUM_POLICY_WARN
        );
        List<RemoteRepository> remotes = new ArrayList<>();
        remotes.add(repository("central", "https://repo.maven.apache.org/maven2", policy));
        for (Profile profile : settings.getProfiles()) {
            boolean active = settings.getActiveProfiles().contains(profile.getId());
            if (!active && profile.getActivation() != null && profile.getActivation().isActiveByDefault()) {
                active = true;
            }
            if (!active) {
                continue;
            }
            for (Repository repository : profile.getRepositories()) {
                if (repository.getUrl() == null || repository.getUrl().isEmpty()) {
                    continue;
                }
                remotes.add(repository(repository.getId(), repository.getUrl(), policy));
            }
        }
        return remotes;
    }

    private static RemoteRepository repository(String id, String url, RepositoryPolicy policy) {
        return new RemoteRepository.Builder(id == null || id.isEmpty() ? "central" : id, "default", url)
            .setReleasePolicy(policy)
            .setSnapshotPolicy(policy)
            .build();
    }

    private static final class ManualWagonProvider implements WagonProvider {
        @Override
        public Wagon lookup(String roleHint) {
            if ("http".equals(roleHint) || "https".equals(roleHint)) {
                return new HttpWagon();
            }
            if ("file".equals(roleHint)) {
                return new FileWagon();
            }
            return null;
        }

        @Override
        public void release(Wagon wagon) {}
    }
}
