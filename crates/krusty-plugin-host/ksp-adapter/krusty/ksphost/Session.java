package krusty.ksphost;

import com.google.devtools.ksp.processing.JvmPlatformInfo;
import com.google.devtools.ksp.processing.PlatformInfo;
import com.google.devtools.ksp.processing.SymbolProcessor;
import com.google.devtools.ksp.processing.SymbolProcessorEnvironment;
import com.google.devtools.ksp.processing.SymbolProcessorProvider;
import java.io.DataInputStream;
import java.io.File;
import java.io.IOException;
import java.net.URL;
import java.net.URLClassLoader;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.ServiceLoader;
import java.util.Set;
import kotlin.KotlinVersion;

// One module's processors, from loading them to their last round: KSP's processing loop, with
// krusty deciding when another round runs.
final class Session {
    final List<String> providers = new ArrayList<>();
    private final List<SymbolProcessor> processors = new ArrayList<>();
    private final URLClassLoader loader;
    private final CodeGen codeGen;
    private final Logger logger;
    private final String moduleName;
    private final Set<String> seenPaths = new HashSet<>();

    private Session(URLClassLoader loader, CodeGen codeGen, Logger logger, String moduleName) {
        this.loader = loader;
        this.codeGen = codeGen;
        this.logger = logger;
        this.moduleName = moduleName;
    }

    // What a round or the finish produced.
    static final class Outcome {
        final List<CodeGen.Generated> generated;
        final List<String> log;
        final boolean hasError;

        Outcome(List<CodeGen.Generated> generated, List<String> log, boolean hasError) {
            this.generated = generated;
            this.log = log;
            this.hasError = hasError;
        }
    }

    static Session load(DataInputStream in) throws IOException {
        List<String> classpath = Wire.readList(in, Wire::readString);
        Map<String, String> options = new LinkedHashMap<>();
        for (String[] option : Wire.readList(in, o -> new String[] {Wire.readString(o), Wire.readString(o)})) {
            options.put(option[0], option[1]);
        }
        String moduleName = Wire.readString(in);
        File kotlinDir = new File(Wire.readString(in));
        File javaDir = new File(Wire.readString(in));
        File classDir = new File(Wire.readString(in));
        File resourceDir = new File(Wire.readString(in));
        KotlinVersion languageVersion = version(Wire.readString(in));
        KotlinVersion apiVersion = version(Wire.readString(in));
        KotlinVersion compilerVersion = version(Wire.readString(in));
        KotlinVersion kspVersion = version(Wire.readString(in));
        String jvmTarget = Wire.readString(in);
        String jvmDefaultMode = Wire.readString(in);
        int logLevel = in.readInt();
        boolean warningsAsErrors = Wire.readBoolean(in);

        URL[] urls = new URL[classpath.size()];
        for (int i = 0; i < urls.length; i++) {
            urls[i] = new File(classpath.get(i)).toURI().toURL();
        }
        URLClassLoader loader = new URLClassLoader(urls, Session.class.getClassLoader());
        CodeGen codeGen = new CodeGen(kotlinDir, javaDir, classDir, resourceDir);
        Logger logger = new Logger(logLevel, warningsAsErrors);
        Session session = new Session(loader, codeGen, logger, moduleName);
        List<PlatformInfo> platforms =
                Collections.singletonList(new JvmPlatform(jvmTarget, jvmDefaultMode));
        SymbolProcessorEnvironment environment =
                new SymbolProcessorEnvironment(
                        options,
                        languageVersion,
                        codeGen,
                        logger,
                        apiVersion,
                        compilerVersion,
                        platforms,
                        kspVersion);
        for (SymbolProcessorProvider provider : ServiceLoader.load(SymbolProcessorProvider.class, loader)) {
            session.providers.add(provider.getClass().getName());
            session.processors.add(provider.create(environment));
        }
        return session;
    }

    // `major.minor` or `major.minor.patch`, with anything after the patch (`-RC`) left out, as
    // KotlinVersion has no place for it.
    private static KotlinVersion version(String text) throws IOException {
        String[] parts = text.split("[.-]");
        try {
            int major = Integer.parseInt(parts[0]);
            int minor = Integer.parseInt(parts[1]);
            int patch = parts.length > 2 && parts[2].chars().allMatch(Character::isDigit) && !parts[2].isEmpty()
                    ? Integer.parseInt(parts[2])
                    : 0;
            return new KotlinVersion(major, minor, patch);
        } catch (RuntimeException error) {
            throw new IOException("not a Kotlin version: " + text);
        }
    }

    Outcome round(Symbols symbols) {
        HostResolver resolver = new HostResolver(symbols, seenPaths, moduleName);
        for (SourceFile file : symbols.files) {
            seenPaths.add(file.getFilePath());
        }
        for (SymbolProcessor processor : processors) {
            processor.process(resolver);
        }
        return new Outcome(codeGen.drain(), logger.drain(), logger.hasError());
    }

    Outcome finish() {
        for (SymbolProcessor processor : processors) {
            if (logger.hasError()) {
                processor.onError();
            } else {
                processor.finish();
            }
        }
        return new Outcome(codeGen.drain(), logger.drain(), logger.hasError());
    }

    void close() throws IOException {
        loader.close();
    }

    private static final class JvmPlatform implements JvmPlatformInfo {
        private final String jvmTarget;
        private final String jvmDefaultMode;

        JvmPlatform(String jvmTarget, String jvmDefaultMode) {
            this.jvmTarget = jvmTarget;
            this.jvmDefaultMode = jvmDefaultMode;
        }

        @Override
        public String getPlatformName() {
            return "JVM";
        }

        @Override
        public String getJvmTarget() {
            return jvmTarget;
        }

        @Override
        public String getJvmDefaultMode() {
            return jvmDefaultMode;
        }

        @Override
        public String toString() {
            return "JVM";
        }
    }
}
