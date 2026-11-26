package krusty.analysis;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import ksp.com.intellij.mock.MockProject;
import ksp.org.jetbrains.kotlin.analysis.api.KaSession;
import ksp.org.jetbrains.kotlin.analysis.api.fir.KaFirSessionProvider;
import ksp.org.jetbrains.kotlin.analysis.api.session.KaSessionProvider;
import ksp.org.jetbrains.kotlin.analysis.api.symbols.KaNamedClassSymbol;
import ksp.org.jetbrains.kotlin.psi.KtClass;
import ksp.org.jetbrains.kotlin.psi.KtFile;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

/**
 * Swaps the Analysis API's session provider in a standalone session. The K2 compiler's own
 * provider is the oracle: it answers the query the same module is then refused under krusty's.
 */
class SessionProviderTest {
    @TempDir
    Path work;

    @Test
    void krustysProviderReplacesFirsAndRefusesWhatItDoesNotServe() throws IOException {
        Path sources = Files.createDirectories(work.resolve("src"));
        Files.writeString(sources.resolve("Greeter.kt"), "package demo\n\nclass Greeter\n");
        try (StandaloneSession standalone = new StandaloneSession(sources, Path.of(System.getProperty("java.home")))) {
            KtFile file = standalone.file("Greeter.kt");
            KtClass greeter = (KtClass) file.getDeclarations().get(0);

            KaSessionProvider fir = KaSessionProvider.Companion.getInstance(standalone.project());
            assertInstanceOf(KaFirSessionProvider.class, fir);
            String oracle = fir.analyze(greeter, session -> {
                KaNamedClassSymbol symbol = session.getNamedClassSymbol(greeter);
                return symbol.getClassId().asString();
            });
            assertEquals("demo/Greeter", oracle);

            KaKrustySessionProvider.install((MockProject) standalone.project());
            KaSessionProvider krusty = KaSessionProvider.Companion.getInstance(standalone.project());
            assertInstanceOf(KaKrustySessionProvider.class, krusty);
            UnsupportedOperationException refusal = assertThrows(UnsupportedOperationException.class,
                () -> krusty.analyze(greeter, (KaSession session) -> session.getNamedClassSymbol(greeter)));
            assertEquals(
                "krusty does not implement KaSymbolProvider.getNamedClassSymbol of the Analysis API yet",
                refusal.getMessage());
        }
    }
}
