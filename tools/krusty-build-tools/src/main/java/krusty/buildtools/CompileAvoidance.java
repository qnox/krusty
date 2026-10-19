package krusty.buildtools;

import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.ArrayList;
import java.util.HexFormat;
import java.util.List;
import java.util.Optional;
import java.util.stream.Stream;

/**
 * Whether a compilation's inputs and output are exactly as its last successful run left them, so
 * the output can stand.
 *
 * The inputs are fingerprinted by the compiler itself ({@code krusty compilation-fingerprint} over
 * the compile command line): krusty reads the command line as the compilation does and digests the
 * contents of every file that compilation selects. The output directory is compared by its files'
 * contents. Nothing is identified by a size or a modification time, and when the fingerprint
 * cannot be taken the compilation runs.
 */
final class CompileAvoidance {
    private static final String STATE_FILE = "krusty-compilation.state";

    private final Path stateFile;
    private final Path destination;

    CompileAvoidance(Path workingDirectory, Path destination) {
        this.stateFile = workingDirectory.resolve(STATE_FILE);
        this.destination = destination;
    }

    /** The fingerprint of the inputs {@code command} (a krusty compile command line) reads, if krusty takes one. */
    static Optional<String> inputs(List<String> command) {
        List<String> fingerprint = new ArrayList<>(command);
        fingerprint.add(1, "compilation-fingerprint");
        try {
            Process process = new ProcessBuilder(fingerprint).redirectErrorStream(true).start();
            process.getOutputStream().close();
            String output;
            try (InputStream input = process.getInputStream()) {
                output = new String(input.readAllBytes(), StandardCharsets.UTF_8).strip();
            }
            if (process.waitFor() == 0 && output.matches("[0-9a-f]{64}")) {
                return Optional.of(output);
            }
            return Optional.empty();
        } catch (IOException e) {
            return Optional.empty();
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
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
            return state.equals(List.of(inputs, outputContents()));
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
            Files.write(stateFile, List.of(inputs, outputContents()), StandardCharsets.UTF_8);
        } catch (IOException e) {
            forget();
        }
    }

    /** Every output file's relative path and contents. */
    private String outputContents() throws IOException {
        MessageDigest tree = sha256();
        List<Path> files;
        try (Stream<Path> walk = Files.walk(destination)) {
            files = walk.filter(Files::isRegularFile).sorted().toList();
        }
        for (Path file : files) {
            field(tree, destination.relativize(file).toString().getBytes(StandardCharsets.UTF_8));
            MessageDigest contents = sha256();
            try (InputStream input = Files.newInputStream(file)) {
                byte[] buffer = new byte[1 << 16];
                for (int read; (read = input.read(buffer)) > 0; ) {
                    contents.update(buffer, 0, read);
                }
            }
            field(tree, contents.digest());
        }
        return HexFormat.of().formatHex(tree.digest());
    }

    /** Length-delimited, so no value can be mistaken for a delimiter. */
    private static void field(MessageDigest digest, byte[] value) {
        digest.update(java.nio.ByteBuffer.allocate(Integer.BYTES).putInt(value.length).array());
        digest.update(value);
    }

    private static MessageDigest sha256() {
        try {
            return MessageDigest.getInstance("SHA-256");
        } catch (NoSuchAlgorithmException e) {
            throw new IllegalStateException("SHA-256 is required of every Java platform", e);
        }
    }
}
