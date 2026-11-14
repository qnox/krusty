package krusty.tooling;

import java.nio.file.Files;
import java.util.List;
import org.apache.maven.repository.internal.MavenRepositorySystemUtils;
import org.apache.maven.settings.Mirror;
import org.apache.maven.settings.Settings;
import org.eclipse.aether.DefaultRepositorySystemSession;
import org.eclipse.aether.RepositorySystem;
import org.eclipse.aether.impl.DefaultServiceLocator;
import org.eclipse.aether.repository.AuthenticationContext;
import org.eclipse.aether.repository.LocalRepository;
import org.eclipse.aether.repository.Proxy;
import org.eclipse.aether.repository.RemoteRepository;
import org.eclipse.aether.repository.RepositoryPolicy;
import org.eclipse.aether.spi.connector.RepositoryConnectorFactory;
import org.eclipse.aether.spi.connector.transport.TransporterFactory;
import org.eclipse.aether.connector.basic.BasicRepositoryConnectorFactory;
import org.eclipse.aether.transport.wagon.WagonProvider;
import org.eclipse.aether.transport.wagon.WagonTransporterFactory;
import org.eclipse.aether.util.repository.AuthenticationBuilder;
import org.eclipse.aether.util.repository.DefaultAuthenticationSelector;
import org.eclipse.aether.util.repository.DefaultProxySelector;
import org.apache.maven.wagon.Wagon;
import org.apache.maven.wagon.providers.file.FileWagon;
import org.apache.maven.wagon.providers.http.HttpWagon;

/**
 * A project repository is normalized by the session before it is used. The mirror, the proxy,
 * and the server credentials are the ones {@code newResolutionRepositories} applies. This does
 * not resolve an artifact.
 */
public final class ProjectRepositoryNormalizationTest {
    private static final String JETBRAINS =
        "https://cache-redirector.jetbrains.com/packages.jetbrains.team/maven/p/ij/intellij-dependencies";
    private static final String MIRROR = "https://mirror.example/maven";
    private static final String CENTRAL = "https://repo.maven.apache.org/maven2";

    private ProjectRepositoryNormalizationTest() {}

    public static void main(String[] args) throws Exception {
        RepositorySystem system = repositorySystem();
        DefaultRepositorySystemSession session = MavenRepositorySystemUtils.newSession();
        session.setLocalRepositoryManager(system.newLocalRepositoryManager(
            session, new LocalRepository(Files.createTempDirectory("krusty-maven-local").toFile())
        ));
        Settings settings = new Settings();
        Mirror mirror = new Mirror();
        mirror.setId("repo-mirror");
        mirror.setUrl(MIRROR);
        mirror.setMirrorOf("intellij-dependencies");
        settings.addMirror(mirror);
        session.setMirrorSelector(MavenResolverClient.mirrorSelector(settings));
        DefaultAuthenticationSelector authentication = new DefaultAuthenticationSelector();
        authentication.add(
            "repo-mirror",
            new AuthenticationBuilder().addUsername("jetbrains").addPassword("secret").build()
        );
        session.setAuthenticationSelector(authentication);
        DefaultProxySelector proxies = new DefaultProxySelector();
        proxies.add(new Proxy("https", "proxy.example", 8443), "localhost|127.0.0.1");
        session.setProxySelector(proxies);

        RepositoryPolicy always = new RepositoryPolicy(
            true, RepositoryPolicy.UPDATE_POLICY_ALWAYS, RepositoryPolicy.CHECKSUM_POLICY_WARN
        );
        RemoteRepository central = new RemoteRepository.Builder("central", "default", CENTRAL).build();
        RemoteRepository project = new RemoteRepository.Builder("intellij-dependencies", "default", JETBRAINS)
            .setReleasePolicy(always)
            .setSnapshotPolicy(always)
            .build();

        List<RemoteRepository> resolved = MavenResolverClient.resolutionRepositories(
            system, session, List.of(central), List.of(project, project)
        );
        expect(resolved.size() == 2, "session repository plus one normalized project repository");
        expect(CENTRAL.equals(resolved.get(0).getUrl()), "the session repository stays as it was");
        RemoteRepository normalized = resolved.get(1);
        expect(MIRROR.equals(normalized.getUrl()), "the project repository is requested through its mirror");
        expect("repo-mirror".equals(normalized.getId()), "the mirror id is the repository that is contacted");
        expect(
            RepositoryPolicy.UPDATE_POLICY_ALWAYS.equals(normalized.getPolicy(false).getUpdatePolicy()),
            "the always update policy survives normalization"
        );
        expect(normalized.getProxy() != null, "the session proxy is applied");
        expect(
            "proxy.example".equals(normalized.getProxy().getHost()),
            "the proxy host comes from the session"
        );
        AuthenticationContext context = AuthenticationContext.forRepository(session, normalized);
        try {
            expect(
                context != null && "jetbrains".equals(context.get(AuthenticationContext.USERNAME)),
                "server credentials are applied to the mirrored repository"
            );
        } finally {
            AuthenticationContext.close(context);
        }
        List<RemoteRepository> unchanged = MavenResolverClient.resolutionRepositories(
            system, session, List.of(central), List.of(central)
        );
        expect(unchanged.size() == 1, "a repository the session already has is not added again");
        System.out.println("ok");
    }

    private static void expect(boolean condition, String message) {
        if (!condition) {
            throw new AssertionError(message);
        }
    }

    private static RepositorySystem repositorySystem() {
        DefaultServiceLocator locator = MavenRepositorySystemUtils.newServiceLocator();
        locator.addService(RepositoryConnectorFactory.class, BasicRepositoryConnectorFactory.class);
        locator.addService(TransporterFactory.class, WagonTransporterFactory.class);
        locator.setServices(WagonProvider.class, new LocalWagonProvider());
        RepositorySystem system = locator.getService(RepositorySystem.class);
        if (system == null) {
            throw new IllegalStateException("RepositorySystem is unavailable");
        }
        return system;
    }

    private static final class LocalWagonProvider implements WagonProvider {
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
