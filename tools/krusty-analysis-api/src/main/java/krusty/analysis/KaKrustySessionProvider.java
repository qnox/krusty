package krusty.analysis;

import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Proxy;
import ksp.com.intellij.mock.MockProject;
import ksp.com.intellij.openapi.project.Project;
import ksp.org.jetbrains.kotlin.analysis.api.KaSession;
import ksp.org.jetbrains.kotlin.analysis.api.projectStructure.KaModule;
import ksp.org.jetbrains.kotlin.analysis.api.session.KaSessionProvider;
import ksp.org.jetbrains.kotlin.psi.KtElement;

/**
 * Hands out the Analysis API's {@link KaSession} for a standalone session, backed by krusty
 * instead of the K2 compiler's FIR.
 *
 * <p>No query is served yet. The session this provider hands out names the first query a caller
 * makes and refuses it with {@link UnsupportedOperationException}, as krusty's KSP host does for
 * the members it does not implement, so a tool never receives an answer krusty has not computed.
 * Queries are served component by component as krusty gains them.
 */
public final class KaKrustySessionProvider extends KaSessionProvider {
    private final KaSession session = refusingSession();

    /**
     * Makes this the provider the project hands to {@code analyze}, in place of the one the
     * standalone session registered (the K2 compiler's). A service is registered once per project,
     * so the registered one is removed first.
     */
    public static KaKrustySessionProvider install(MockProject project) {
        KaKrustySessionProvider provider = new KaKrustySessionProvider(project);
        project.getPicoContainer().unregisterComponent(KaSessionProvider.class.getName());
        project.registerService(KaSessionProvider.class, provider);
        return provider;
    }

    public KaKrustySessionProvider(Project project) {
        super(project);
    }

    @Override
    public KaSession getAnalysisSession(KtElement useSiteElement) {
        return session;
    }

    @Override
    public KaSession getAnalysisSession(KaModule useSiteModule) {
        return session;
    }

    @Override
    public void beforeEnteringAnalysis(KaSession session, KtElement useSiteElement) {
    }

    @Override
    public void beforeEnteringAnalysis(KaSession session, KaModule useSiteModule) {
    }

    @Override
    public Void handleAnalysisException(Throwable exception, KaSession session, KtElement useSiteElement) {
        throw rethrow(exception);
    }

    @Override
    public Void handleAnalysisException(Throwable exception, KaSession session, KaModule useSiteModule) {
        throw rethrow(exception);
    }

    @Override
    public void afterLeavingAnalysis(KaSession session, KtElement useSiteElement) {
    }

    @Override
    public void afterLeavingAnalysis(KaSession session, KaModule useSiteModule) {
    }

    @Override
    public void clearCaches() {
    }

    private static RuntimeException rethrow(Throwable exception) {
        if (exception instanceof RuntimeException runtime) {
            return runtime;
        }
        if (exception instanceof Error error) {
            throw error;
        }
        return new IllegalStateException(exception);
    }

    /** A session whose every member refuses, named by the interface that declares it. */
    private static KaSession refusingSession() {
        InvocationHandler refuse = (proxy, method, arguments) -> {
            switch (method.getName() + "/" + method.getParameterCount()) {
                case "toString/0":
                    return "KaKrustySession";
                case "hashCode/0":
                    return System.identityHashCode(proxy);
                case "equals/1":
                    return proxy == arguments[0];
                default:
                    throw new UnsupportedOperationException(
                        "krusty does not implement " + method.getDeclaringClass().getSimpleName()
                            + "." + method.getName() + " of the Analysis API yet");
            }
        };
        return (KaSession) Proxy.newProxyInstance(
            KaKrustySessionProvider.class.getClassLoader(), new Class<?>[] {KaSession.class}, refuse);
    }
}
