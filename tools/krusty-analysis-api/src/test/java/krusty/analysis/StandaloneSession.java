package krusty.analysis;

import java.nio.file.Path;
import ksp.com.intellij.openapi.Disposable;
import ksp.com.intellij.openapi.project.Project;
import ksp.com.intellij.openapi.util.Disposer;
import ksp.com.intellij.mock.MockApplication;
import ksp.org.jetbrains.kotlin.analysis.api.impl.base.permissions.KaBaseAnalysisPermissionRegistry;
import ksp.org.jetbrains.kotlin.analysis.api.permissions.KaAnalysisPermissionRegistry;
import ksp.org.jetbrains.kotlin.analysis.api.platform.permissions.KotlinAnalysisPermissionOptions;
import ksp.org.jetbrains.kotlin.analysis.api.platform.resolution.KaResolutionActivityTracker;
import ksp.org.jetbrains.kotlin.analysis.api.standalone.base.permissions.KotlinStandaloneAnalysisPermissionOptions;
import ksp.org.jetbrains.kotlin.analysis.low.level.api.fir.lazy.resolve.LLFirResolutionActivityTracker;
import ksp.org.jetbrains.kotlin.analysis.api.projectStructure.KaModule;
import ksp.org.jetbrains.kotlin.analysis.api.projectStructure.KaSourceModule;
import ksp.org.jetbrains.kotlin.analysis.api.standalone.StandaloneAnalysisAPISession;
import ksp.org.jetbrains.kotlin.analysis.api.standalone.StandaloneAnalysisAPISessionBuilderKt;
import ksp.org.jetbrains.kotlin.analysis.project.structure.builder.KaSdkModuleBuilderKt;
import ksp.org.jetbrains.kotlin.analysis.project.structure.builder.KaSourceModuleBuilderKt;
import ksp.org.jetbrains.kotlin.platform.jvm.JvmPlatforms;
import ksp.org.jetbrains.kotlin.psi.KtFile;
import kotlin.Unit;

/**
 * A standalone Analysis API session over one source module, as KSP2 builds it: the module's
 * sources and the JDK, on JetBrains' module model. The session's service for handing out
 * analysis sessions is the one a test sets, so the same module is analysed once by the K2
 * compiler's FIR and once by krusty's provider.
 */
final class StandaloneSession implements AutoCloseable {
    private final Disposable disposable = Disposer.newDisposable("krusty standalone session");
    final StandaloneAnalysisAPISession session;
    final KaSourceModule module;

    StandaloneSession(Path sourceRoot, Path jdkHome) {
        KaSourceModule[] built = new KaSourceModule[1];
        session = StandaloneAnalysisAPISessionBuilderKt.buildStandaloneAnalysisAPISession(
            disposable, false, builder -> {
                // The services KSP registers itself when it builds its session: the standalone
                // builder leaves them to its caller.
                MockApplication application = (MockApplication) builder.getApplication();
                application.registerService(
                    KaAnalysisPermissionRegistry.class, KaBaseAnalysisPermissionRegistry.class);
                application.registerService(
                    KotlinAnalysisPermissionOptions.class, KotlinStandaloneAnalysisPermissionOptions.class);
                application.registerService(
                    KaResolutionActivityTracker.class, LLFirResolutionActivityTracker.class);
                builder.buildKtModuleProvider(providers -> {
                    providers.setPlatform(JvmPlatforms.INSTANCE.getDefaultJvmPlatform());
                    KaModule jdk = KaSdkModuleBuilderKt.buildKtSdkModule(providers, sdk -> {
                        sdk.setPlatform(JvmPlatforms.INSTANCE.getDefaultJvmPlatform());
                        sdk.addBinaryRootsFromJdkHome(jdkHome, true);
                        sdk.setLibraryName("jdk");
                        return Unit.INSTANCE;
                    });
                    built[0] = KaSourceModuleBuilderKt.buildKtSourceModule(providers, source -> {
                        source.setPlatform(JvmPlatforms.INSTANCE.getDefaultJvmPlatform());
                        source.setModuleName("main");
                        source.addSourceRoot(sourceRoot);
                        source.addRegularDependency(jdk);
                        return Unit.INSTANCE;
                    });
                    providers.addModule(built[0]);
                    return Unit.INSTANCE;
                });
                return Unit.INSTANCE;
            });
        module = built[0];
    }

    Project project() {
        return session.getProject();
    }

    /** The one file of the module with this name. */
    KtFile file(String name) {
        return session.getModulesWithFiles().get(module).stream()
            .filter(file -> file.getName().equals(name))
            .map(KtFile.class::cast)
            .findFirst()
            .orElseThrow(() -> new IllegalArgumentException("no file " + name));
    }

    @Override
    public void close() {
        // Only this session's own services: the application services are JVM-wide and shared by
        // every other session of the process, so a later session must still find them running.
        Disposer.dispose(disposable);
    }
}
