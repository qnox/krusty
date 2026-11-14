package krusty.tooling;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import org.eclipse.aether.repository.AuthenticationContext;
import org.eclipse.aether.repository.RemoteRepository;
import org.eclipse.aether.repository.RepositoryPolicy;

/**
 * The resolver session is the one Maven builds from settings.xml. A jarRepositories.xml
 * entry is added through that session, which applies the mirror, the proxy, and the server
 * credentials. This does not resolve an artifact.
 */
public final class ProjectRepositoryNormalizationTest {
    private static final String JETBRAINS =
        "https://cache-redirector.jetbrains.com/packages.jetbrains.team/maven/p/ij/intellij-dependencies";
    private static final String MIRROR = "https://mirror.example/maven";
    private static final String CENTRAL = "https://repo.maven.apache.org/maven2";

    private ProjectRepositoryNormalizationTest() {}

    public static void main(String[] args) throws Exception {
        Path directory = Files.createTempDirectory("krusty-maven-settings");
        Path local = directory.resolve("repository");
        Files.createDirectories(local);
        Path settings = directory.resolve("settings.xml");
        Files.writeString(settings, """
            <settings>
              <localRepository>%s</localRepository>
              <mirrors>
                <mirror>
                  <id>repo-mirror</id>
                  <url>%s</url>
                  <mirrorOf>intellij-dependencies</mirrorOf>
                </mirror>
              </mirrors>
              <servers>
                <server>
                  <id>repo-mirror</id>
                  <username>jetbrains</username>
                  <password>secret</password>
                </server>
              </servers>
              <proxies>
                <proxy>
                  <id>example</id>
                  <active>true</active>
                  <protocol>https</protocol>
                  <host>proxy.example</host>
                  <port>8443</port>
                  <nonProxyHosts>localhost|127.0.0.1</nonProxyHosts>
                </proxy>
              </proxies>
            </settings>
            """.formatted(local.toAbsolutePath(), MIRROR));

        MavenResolverClient.MavenRuntime runtime =
            MavenResolverClient.open(settings.toFile(), null);
        try {
            expect(
                local.toFile().getCanonicalFile().equals(
                    runtime.session.getLocalRepository().getBasedir().getCanonicalFile()
                ),
                "the local repository is the one settings.xml names"
            );
            expect(runtime.repositories.size() == 1, "Maven's default central repository is the session list");
            expect(
                CENTRAL.equals(runtime.repositories.get(0).getUrl()),
                "central comes from Maven's request, not a repository this test adds"
            );

            List<Map<String, String>> projects = List.of(
                Map.of("id", "intellij-dependencies", "url", JETBRAINS),
                Map.of("id", "intellij-dependencies", "url", JETBRAINS)
            );
            List<RemoteRepository> resolved = MavenResolverClient.withProjectRepositories(runtime, projects);
            expect(resolved.size() == 2, "session repository plus one normalized project repository");
            expect(CENTRAL.equals(resolved.get(0).getUrl()), "the session repository stays as Maven resolved it");
            RemoteRepository normalized = resolved.get(1);
            expect(MIRROR.equals(normalized.getUrl()), "the project repository is requested through its mirror");
            expect("repo-mirror".equals(normalized.getId()), "the mirror id is the repository that is contacted");
            expect(
                RepositoryPolicy.UPDATE_POLICY_ALWAYS.equals(normalized.getPolicy(false).getUpdatePolicy()),
                "the always update policy survives the session"
            );
            expect(normalized.getProxy() != null, "the settings proxy is applied");
            expect(
                "proxy.example".equals(normalized.getProxy().getHost()),
                "the proxy host comes from settings.xml"
            );
            AuthenticationContext context = AuthenticationContext.forRepository(runtime.session, normalized);
            try {
                expect(
                    context != null && "jetbrains".equals(context.get(AuthenticationContext.USERNAME)),
                    "server credentials from settings.xml are applied to the mirrored repository"
                );
            } finally {
                AuthenticationContext.close(context);
            }
        } finally {
            runtime.close();
        }
        System.out.println("ok");
    }

    private static void expect(boolean condition, String message) {
        if (!condition) {
            throw new AssertionError(message);
        }
    }
}
