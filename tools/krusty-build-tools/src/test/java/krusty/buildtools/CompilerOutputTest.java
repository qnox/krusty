package krusty.buildtools;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import org.jetbrains.kotlin.buildtools.api.KotlinLogger;
import org.junit.jupiter.api.Test;

class CompilerOutputTest {
    @Test
    void handsTheCallerTheLineAndAbsolutePathInsteadOfKrustysExcerptAndRelativePath() throws IOException {
        Path source = Path.of("CompilerOutputTestSource.kt").toAbsolutePath();
        Files.writeString(source, "fun broken(): Int = missing()\n");
        try {
            String output = "CompilerOutputTestSource.kt:1:21: error: unresolved reference 'missing'.\n"
                + "fun broken(): Int = missing()\n"
                + "                    ^^^^^^^\n";
            List<String> rendered = new ArrayList<>();
            CompilerOutput parsed = new CompilerOutput((severity, message, location) -> {
                rendered.add(location.getPath() + ":" + location.getLine() + " [" + location.getLineContent() + "] " + message);
                return message;
            });
            parsed.read(new ByteArrayInputStream(output.getBytes(StandardCharsets.UTF_8)));
            parsed.report(new Ignoring());
            assertEquals(
                List.of(source + ":1 [fun broken(): Int = missing()] unresolved reference 'missing'."),
                rendered);
        } finally {
            Files.deleteIfExists(source);
        }
    }

    private static final class Ignoring implements KotlinLogger {
        @Override
        public boolean isDebugEnabled() {
            return false;
        }

        @Override
        public void error(String message, Throwable throwable) {
        }

        @Override
        public void warn(String message, Throwable throwable) {
        }

        @Override
        public void info(String message) {
        }

        @Override
        public void debug(String message) {
        }

        @Override
        public void lifecycle(String message) {
        }
    }
}
