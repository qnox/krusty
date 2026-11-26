package krusty.analysis;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.stream.Stream;
import ksp.org.jetbrains.kotlin.psi.KtClass;
import ksp.org.jetbrains.kotlin.psi.KtDeclaration;
import ksp.org.jetbrains.kotlin.psi.KtEnumEntry;
import ksp.org.jetbrains.kotlin.psi.KtFile;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

/**
 * The source ranges kotlinc's PSI gives declarations, which krusty's parser has to reproduce. Each
 * fixture under {@code tests/psi_ranges} is a Kotlin source with the text of every declaration of
 * a kind as PSI delimits it; krusty's parser tests read the same files. Set
 * {@code KRUSTY_PSI_REGENERATE} to rewrite the expectations from PSI after adding a source.
 */
class PsiRangesTest {
    private static final Path FIXTURES = Path.of("..", "..", "tests", "psi_ranges");

    @TempDir
    Path work;

    @Test
    void enumEntriesHaveTheRangesTheParserRecords() throws IOException {
        Path directory = FIXTURES.resolve("enum_entries");
        List<Path> sources;
        try (Stream<Path> files = Files.list(directory)) {
            sources = files.filter(path -> path.toString().endsWith(".kt")).sorted().toList();
        }
        Path root = Files.createDirectories(work.resolve("src"));
        for (Path source : sources) {
            Files.copy(source, root.resolve(source.getFileName()));
        }
        boolean regenerate = System.getenv("KRUSTY_PSI_REGENERATE") != null;
        try (StandaloneSession session = new StandaloneSession(root, Path.of(System.getProperty("java.home")))) {
            for (Path source : sources) {
                KtFile file = session.file(source.getFileName().toString());
                StringBuilder actual = new StringBuilder();
                for (KtDeclaration declaration : file.getDeclarations()) {
                    if (declaration instanceof KtClass type) {
                        for (KtDeclaration member : type.getDeclarations()) {
                            if (member instanceof KtEnumEntry entry) {
                                actual.append(entry.getName()).append('\t')
                                    .append(escape(entry.getText())).append('\n');
                            }
                        }
                    }
                }
                Path expected = directory.resolve(source.getFileName().toString().replaceAll("\\.kt$", ".expected"));
                if (regenerate || !Files.exists(expected)) {
                    Files.writeString(expected, actual.toString());
                }
                assertEquals(Files.readString(expected), actual.toString(), source.getFileName().toString());
            }
        }
    }

    private static String escape(String text) {
        return text.replace("\\", "\\\\").replace("\n", "\\n").replace("\t", "\\t").replace("\r", "\\r");
    }
}
