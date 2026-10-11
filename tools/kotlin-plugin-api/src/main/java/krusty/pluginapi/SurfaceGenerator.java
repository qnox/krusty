package krusty.pluginapi;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.jar.Attributes;
import java.util.jar.JarEntry;
import java.util.jar.JarOutputStream;
import java.util.jar.Manifest;

/**
 * Builds the Kotlin compiler plugin API surface jar.
 *
 * <pre>
 * SurfaceGenerator --compiler kotlin-compiler.jar --stdlib kotlin-stdlib.jar --kotlin-version 2.4.20 --out surface.jar
 *                  --report report.md plugin.jar...
 * </pre>
 *
 * The surface is derived, never hand-written: it is exactly the part of the reference compiler that
 * the given plugin jars link against, so it tracks each Kotlin release by regeneration.
 */
public final class SurfaceGenerator {
    private SurfaceGenerator() {
    }

    public static void main(String[] args) throws IOException {
        Path compilerJar = null;
        Path stdlibJar = null;
        Path out = null;
        Path report = null;
        String kotlinVersion = "unknown";
        List<Path> plugins = new ArrayList<>();
        for (int i = 0; i < args.length; i++) {
            switch (args[i]) {
                case "--compiler" -> compilerJar = Path.of(args[++i]);
                case "--stdlib" -> stdlibJar = Path.of(args[++i]);
                case "--out" -> out = Path.of(args[++i]);
                case "--report" -> report = Path.of(args[++i]);
                case "--kotlin-version" -> kotlinVersion = args[++i];
                default -> plugins.add(Path.of(args[i]));
            }
        }
        if (compilerJar == null || stdlibJar == null || out == null || plugins.isEmpty()) {
            throw new IllegalArgumentException("usage: --compiler <jar> --stdlib <jar> --out <jar> [--report <md>] [--kotlin-version <v>] <plugin.jar>...");
        }
        try (ClassIndex compiler = new ClassIndex(compilerJar, false);
             ClassIndex stdlib = new ClassIndex(stdlibJar, false)) {
            Map<String, PluginReferences> references = new LinkedHashMap<>();
            for (Path p : plugins) {
                try (ClassIndex jar = new ClassIndex(p, true)) {
                    references.put(p.getFileName().toString(), PluginReferences.scan(jar));
                }
            }
            SurfaceModel model = new SurfaceModel(compiler, stdlib, references);
            Map<String, byte[]> entries = new TreeMap<>();
            StubWriter stubs = new StubWriter(model);
            for (String name : model.contracts.keySet()) {
                entries.put(name + ".class", stubs.write(name));
            }
            for (String name : model.data) {
                entries.put(name + ".class", compiler.bytes(name));
            }
            String support = StubWriter.SURFACE + ".class";
            try (InputStream in = SurfaceGenerator.class.getResourceAsStream("/" + support)) {
                entries.put(support, in.readAllBytes());
            }
            writeJar(out, entries, kotlinVersion);
            if (report != null) {
                Files.writeString(report, report(model, references, kotlinVersion));
            }
            System.out.printf("%s: %d contract types (%d members), %d data classes%n", out,
                    model.contracts.size(), model.contracts.values().stream().mapToInt(java.util.Set::size).sum(),
                    model.data.size());
        }
    }

    /** A reproducible jar: sorted entries with a fixed timestamp. */
    private static void writeJar(Path out, Map<String, byte[]> entries, String kotlinVersion) throws IOException {
        Manifest manifest = new Manifest();
        manifest.getMainAttributes().put(Attributes.Name.MANIFEST_VERSION, "1.0");
        manifest.getMainAttributes().putValue("Implementation-Title", "kotlin-plugin-api-surface");
        manifest.getMainAttributes().putValue("Kotlin-Compiler-Version", kotlinVersion);
        Files.createDirectories(out.toAbsolutePath().getParent());
        try (OutputStream file = Files.newOutputStream(out); JarOutputStream jar = new JarOutputStream(file)) {
            JarEntry m = new JarEntry("META-INF/MANIFEST.MF");
            m.setTime(0L);
            jar.putNextEntry(m);
            manifest.write(jar);
            jar.closeEntry();
            for (var e : entries.entrySet()) {
                JarEntry entry = new JarEntry(e.getKey());
                entry.setTime(0L);
                jar.putNextEntry(entry);
                jar.write(e.getValue());
                jar.closeEntry();
            }
        }
    }

    private static String report(SurfaceModel model, Map<String, PluginReferences> references, String kotlinVersion) {
        StringBuilder r = new StringBuilder();
        r.append("# Kotlin compiler plugin API surface (Kotlin ").append(kotlinVersion).append(")\n\n");
        int members = model.contracts.values().stream().mapToInt(java.util.Set::size).sum();
        r.append("- contract types: ").append(model.contracts.size()).append(" (").append(members).append(" members)\n");
        r.append("- of which plugins extend or implement: ").append(model.extensible.size()).append('\n');
        r.append("- data classes copied whole: ").append(model.data.size()).append("\n\n");
        r.append("| plugin | compiler types named | member references | not in this compiler |\n|---|---:|---:|---:|\n");
        for (var e : references.entrySet()) {
            PluginReferences p = e.getValue();
            long types = p.types.stream().filter(model.compiler::contains).count();
            long refs = p.refs.stream().filter(x -> model.compiler.contains(x.owner())).count();
            int missing = model.unresolved.getOrDefault(e.getKey(), List.of()).size();
            r.append("| ").append(e.getKey()).append(" | ").append(types).append(" | ").append(refs)
                    .append(" | ").append(missing).append(" |\n");
        }
        for (var e : model.unresolved.entrySet()) {
            r.append("\n## References ").append(e.getKey()).append(" makes that this compiler lacks\n\n");
            e.getValue().stream().map(PluginReferences.Ref::member).distinct().sorted()
                    .forEach(m -> r.append("- `").append(m).append("`\n"));
        }
        return r.toString();
    }
}
