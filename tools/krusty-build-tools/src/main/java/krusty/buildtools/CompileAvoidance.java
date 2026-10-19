package krusty.buildtools;

import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.attribute.BasicFileAttributes;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.ArrayList;
import java.util.HexFormat;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.concurrent.ConcurrentHashMap;
import java.util.stream.Stream;

/**
 * Whether a compilation's inputs are unchanged since its last successful run, so its output can
 * stand. The inputs are the source files' contents, every compiler argument's spelling, the krusty
 * binary and Kotlin release, and what the compilation observes of each classpath, friend-path and
 * compiler-plugin entry:
 *
 * <ul>
 *   <li>a directory of compiled classes, by {@code krusty dependency-fingerprint}: an upstream
 *       module's body-only edit leaves it unchanged, so its dependents are not recompiled;</li>
 *   <li>a Java source root, by every file's contents;</li>
 *   <li>a file such as a jar, by its contents.</li>
 * </ul>
 *
 * Any other path an argument names (a JDK home, for instance) enters by its spelling only. The
 * output directory must also be exactly as the last run left it. When anything cannot be read,
 * the compilation runs.
 */
final class CompileAvoidance {
    private static final String STATE_FILE = "krusty-compilation.state";
    /** Bumped whenever the inputs read here change, so a state file from an older jar never matches. */
    private static final String FORMAT = "krusty-compile-avoidance 1";

    /** Directory fingerprints, keyed by the directory and its file tree's sizes and times. */
    private static final Map<String, String> DIRECTORIES = new ConcurrentHashMap<>();
    /** File contents digests, keyed by the path, size and modification time. */
    private static final Map<String, String> FILES = new ConcurrentHashMap<>();

    private final Path stateFile;
    private final Path destination;

    CompileAvoidance(Path workingDirectory, Path destination) {
        this.stateFile = workingDirectory.resolve(STATE_FILE);
        this.destination = destination;
    }

    /** The inputs' fingerprint, or empty when one of them cannot be read. */
    Optional<String> inputs(Path binary, String kotlinVersion, List<String> arguments, List<Path> sources) {
        try {
            Fingerprint fingerprint = new Fingerprint();
            fingerprint.add("format", FORMAT);
            fingerprint.add("kotlin", kotlinVersion);
            fingerprint.add("binary", binary.toAbsolutePath().toString());
            fingerprint.add("binary-contents", fileDigest(binary));
            fingerprint.add("destination", destination.toAbsolutePath().toString());
            fingerprint.count("arguments", arguments.size());
            for (String argument : arguments) {
                fingerprint.add("argument", argument);
            }
            List<String> entries = observedEntries(arguments);
            fingerprint.count("entries", entries.size());
            for (String entry : entries) {
                fingerprint.add("entry", entry);
                fingerprint.add("observed", entryDigest(binary, entry));
            }
            fingerprint.count("sources", sources.size());
            for (Path source : sources) {
                fingerprint.add("source", source.toString());
                fingerprint.add("source-contents", contentsDigest(source));
            }
            return Optional.of(fingerprint.finish());
        } catch (IOException | InterruptedException e) {
            if (e instanceof InterruptedException) {
                Thread.currentThread().interrupt();
            }
            return Optional.empty();
        }
    }

    /** Whether the last successful run had these inputs and left the output as it is now. */
    boolean upToDate(String inputs) {
        try {
            if (!Files.isRegularFile(stateFile) || !Files.isDirectory(destination)) {
                return false;
            }
            List<String> state = Files.readAllLines(stateFile, StandardCharsets.UTF_8);
            return state.equals(List.of(inputs, treeState(destination)));
        } catch (IOException e) {
            return false;
        }
    }

    /** Forgets the last run, before a compilation that may fail or be cancelled midway. */
    void forget() {
        try {
            Files.deleteIfExists(stateFile);
        } catch (IOException e) {
            // A state file that cannot be removed cannot be trusted either; the next check rereads it.
        }
    }

    /** Records a successful run with these inputs and the output it left. */
    void record(String inputs) {
        try {
            Files.createDirectories(stateFile.getParent());
            Files.write(stateFile, List.of(inputs, treeState(destination)), StandardCharsets.UTF_8);
        } catch (IOException e) {
            forget();
        }
    }

    /**
     * The entries whose contents a compilation observes beyond their spelling: each classpath
     * ({@code -classpath}/{@code -cp}, as {@code -classpath=v} or {@code -classpath v}) and friend
     * path, each compiler plugin jar and each Java source root.
     */
    static List<String> observedEntries(List<String> arguments) {
        List<String> entries = new ArrayList<>();
        for (int index = 0; index < arguments.size(); index++) {
            String argument = arguments.get(index);
            String name = argument;
            String value = null;
            int equals = argument.indexOf('=');
            if (equals > 0) {
                name = argument.substring(0, equals);
                value = argument.substring(equals + 1);
            }
            boolean classpath = name.equals("-classpath") || name.equals("-cp");
            if (classpath && value == null && index + 1 < arguments.size()) {
                value = arguments.get(++index);
            }
            if (value == null) {
                continue;
            }
            String separator = switch (name) {
                case "-classpath", "-cp" -> java.io.File.pathSeparator;
                case "-Xfriend-paths", "-Xplugin", "-Xjava-source-roots" -> ",";
                default -> null;
            };
            if (separator == null) {
                continue;
            }
            for (String entry : value.split(java.util.regex.Pattern.quote(separator), -1)) {
                if (!entry.isEmpty()) {
                    entries.add(name + "\0" + entry);
                }
            }
        }
        return entries;
    }

    private static String entryDigest(Path binary, String entry) throws IOException, InterruptedException {
        int split = entry.indexOf('\0');
        String argument = entry.substring(0, split);
        Path path = Path.of(entry.substring(split + 1));
        if (Files.isRegularFile(path)) {
            return fileDigest(path);
        }
        if (!Files.isDirectory(path)) {
            return "absent";
        }
        if (argument.equals("-Xjava-source-roots")) {
            return treeContents(path);
        }
        return directoryFingerprint(binary, path);
    }

    private static String directoryFingerprint(Path binary, Path directory) throws IOException, InterruptedException {
        String key = binary.toAbsolutePath() + "\0" + directory.toAbsolutePath() + "\0" + treeState(directory);
        String cached = DIRECTORIES.get(key);
        if (cached != null) {
            return cached;
        }
        Process process = new ProcessBuilder(binary.toString(), "dependency-fingerprint", directory.toString())
            .redirectErrorStream(true)
            .start();
        process.getOutputStream().close();
        String output;
        try (InputStream input = process.getInputStream()) {
            output = new String(input.readAllBytes(), StandardCharsets.UTF_8).strip();
        }
        if (process.waitFor() != 0 || !output.matches("[0-9a-f]{64}")) {
            throw new IOException("krusty dependency-fingerprint " + directory + " failed: " + output);
        }
        DIRECTORIES.put(key, output);
        return output;
    }

    private static String fileDigest(Path file) throws IOException {
        BasicFileAttributes attributes = Files.readAttributes(file, BasicFileAttributes.class);
        String key = file.toAbsolutePath() + "\0" + attributes.size() + "\0" + attributes.lastModifiedTime().toMillis();
        String cached = FILES.get(key);
        if (cached != null) {
            return cached;
        }
        String digest = contentsDigest(file);
        FILES.put(key, digest);
        return digest;
    }

    private static String contentsDigest(Path file) throws IOException {
        MessageDigest digest = sha256();
        try (InputStream input = Files.newInputStream(file)) {
            byte[] buffer = new byte[1 << 16];
            for (int read; (read = input.read(buffer)) > 0; ) {
                digest.update(buffer, 0, read);
            }
        }
        return HexFormat.of().formatHex(digest.digest());
    }

    /** Every file's relative path and contents digest. */
    private static String treeContents(Path root) throws IOException {
        Fingerprint fingerprint = new Fingerprint();
        for (Path file : files(root)) {
            fingerprint.add("file", root.relativize(file).toString());
            fingerprint.add("contents", fileDigest(file));
        }
        return fingerprint.finish();
    }

    /** Every file's relative path, size and modification time: cheap evidence the tree is unchanged. */
    private static String treeState(Path root) throws IOException {
        Fingerprint fingerprint = new Fingerprint();
        for (Path file : files(root)) {
            BasicFileAttributes attributes = Files.readAttributes(file, BasicFileAttributes.class);
            fingerprint.add("file", root.relativize(file).toString());
            fingerprint.add("size", Long.toString(attributes.size()));
            fingerprint.add("modified", Long.toString(attributes.lastModifiedTime().toMillis()));
        }
        return fingerprint.finish();
    }

    private static List<Path> files(Path root) throws IOException {
        try (Stream<Path> walk = Files.walk(root)) {
            return walk.filter(Files::isRegularFile).sorted().toList();
        }
    }

    private static MessageDigest sha256() {
        try {
            return MessageDigest.getInstance("SHA-256");
        } catch (NoSuchAlgorithmException e) {
            throw new IllegalStateException("SHA-256 is required of every Java platform", e);
        }
    }

    /** Length-delimited fields, so no value can be mistaken for a delimiter. */
    private static final class Fingerprint {
        private final MessageDigest digest = sha256();

        void add(String tag, String value) {
            field(tag, value.getBytes(StandardCharsets.UTF_8));
        }

        void count(String tag, int value) {
            add(tag, Integer.toString(value));
        }

        private void field(String tag, byte[] value) {
            byte[] name = tag.getBytes(StandardCharsets.UTF_8);
            digest.update(length(name.length));
            digest.update(name);
            digest.update(length(value.length));
            digest.update(value);
        }

        private static byte[] length(int length) {
            return java.nio.ByteBuffer.allocate(Integer.BYTES).putInt(length).array();
        }

        String finish() {
            return HexFormat.of().formatHex(digest.digest());
        }
    }
}
