package krusty.buildtools;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Properties;

/**
 * Where the krusty binary is and which Kotlin release it is a drop-in for.
 *
 * The binary comes from the {@code krusty.binary} system property or the {@code KRUSTY_BIN}
 * environment variable, the same names the krusty Gradle plugin reads. The Kotlin version is fixed
 * when this jar is built: a build tool picks a Build Tools API implementation by Kotlin version, so
 * one jar stands in for exactly one release.
 */
record KrustyConfiguration(String kotlinVersion) {
    private static final String RESOURCE = "krusty-build-tools.properties";

    static KrustyConfiguration load() {
        Properties properties = new Properties();
        try (InputStream input = KrustyConfiguration.class.getResourceAsStream(RESOURCE)) {
            if (input == null) {
                throw new IllegalStateException("missing " + RESOURCE + " in the krusty Build Tools API jar");
            }
            properties.load(input);
        } catch (IOException e) {
            throw new IllegalStateException("cannot read " + RESOURCE, e);
        }
        String version = properties.getProperty("kotlin.version");
        if (version == null || version.isBlank()) {
            throw new IllegalStateException(RESOURCE + " does not name kotlin.version");
        }
        return new KrustyConfiguration(version);
    }

    /** Resolved per operation, so a binary set after the implementation was loaded is honored. */
    Path binary() {
        String configured = System.getProperty("krusty.binary");
        if (configured == null || configured.isBlank()) {
            configured = System.getenv("KRUSTY_BIN");
        }
        if (configured == null || configured.isBlank()) {
            throw new IllegalStateException(
                "set KRUSTY_BIN (or the krusty.binary system property) to the krusty binary");
        }
        Path binary = Path.of(configured);
        if (!Files.isExecutable(binary)) {
            throw new IllegalStateException("krusty binary is not executable: " + binary);
        }
        return binary;
    }
}
