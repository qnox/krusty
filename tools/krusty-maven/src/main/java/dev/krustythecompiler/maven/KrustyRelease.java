package dev.krustythecompiler.maven;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.URI;
import java.net.URLConnection;
import java.nio.channels.FileChannel;
import java.nio.channels.FileLock;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.StandardCopyOption;
import java.nio.file.StandardOpenOption;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.Locale;
import java.util.zip.GZIPInputStream;
import java.util.zip.ZipEntry;
import java.util.zip.ZipInputStream;
import org.apache.maven.plugin.MojoExecutionException;

/**
 * The krusty compiler of this plugin's release, downloaded once from the release into the local
 * repository's cache directory and checked against the release's {@code SHA256SUMS}, as the Gradle
 * plugin does with the Gradle user home.
 */
final class KrustyRelease {
    /** Where krusty releases live; {@code -Dkrusty.releases=<url>} points at a mirror instead. */
    static final String GITHUB_RELEASES = "https://github.com/qnox/krusty/releases/download";

    private KrustyRelease() {}

    /** The release this plugin jar was published with, or null for a build without one. */
    static String pluginRelease() {
        Package source = KrustyRelease.class.getPackage();
        return source == null ? null : source.getImplementationVersion();
    }

    /**
     * The installs of this JVM, one at a time: a parallel build runs the goal on several threads,
     * and a second {@link FileChannel#lock()} on one file from the same JVM throws rather than
     * waits. The file lock then orders installs across JVMs.
     */
    private static final Object INSTALL = new Object();

    static File compiler(String release, String releases, File cache, boolean offline)
            throws MojoExecutionException {
        synchronized (INSTALL) {
            return installed(release, releases, cache, offline);
        }
    }

    private static File installed(String release, String releases, File cache, boolean offline)
            throws MojoExecutionException {
        String triple = hostTriple();
        boolean windows = triple.endsWith("windows-msvc");
        File directory = new File(cache, release + "/" + triple);
        File executable = new File(directory, windows ? "krusty.exe" : "krusty");
        if (executable.isFile()) return executable;
        if (offline) {
            throw new MojoExecutionException("the krusty " + release + " compiler for " + triple + " is not in "
                    + directory + " and Maven is offline; run once online or set -Dkrusty.binary=<path>");
        }
        if (!directory.isDirectory() && !directory.mkdirs()) {
            throw new MojoExecutionException("cannot create " + directory);
        }
        try (FileChannel channel = FileChannel.open(new File(directory, ".lock").toPath(),
                StandardOpenOption.CREATE, StandardOpenOption.WRITE);
             FileLock ignored = channel.lock()) {
            if (!executable.isFile()) publish(release, releases, triple, windows, directory, executable);
        } catch (IOException e) {
            throw new MojoExecutionException("cannot install the krusty " + release + " compiler in " + directory, e);
        }
        return executable;
    }

    private static void publish(String release, String releases, String triple, boolean windows, File directory,
            File executable) throws MojoExecutionException, IOException {
        String archive = "krusty-" + release + "-" + triple + (windows ? ".zip" : ".tar.gz");
        String expected = expectedSha256(releases, release, archive);
        byte[] bytes = download(releases + "/v" + release + "/" + archive);
        String actual = sha256(bytes);
        if (!actual.equals(expected)) {
            throw new MojoExecutionException(archive + " from the krusty " + release + " release has SHA-256 "
                    + actual + ", expected " + expected);
        }
        File staged = new File(directory, executable.getName() + ".part");
        try (InputStream entry = windows ? zipEntry(bytes, executable.getName()) : tarEntry(bytes, executable.getName());
             OutputStream out = Files.newOutputStream(staged.toPath())) {
            entry.transferTo(out);
        }
        if (!staged.setExecutable(true)) throw new IOException("cannot make " + staged + " executable");
        Files.move(staged.toPath(), executable.toPath(), StandardCopyOption.ATOMIC_MOVE);
    }

    static String hostTriple() throws MojoExecutionException {
        String os = System.getProperty("os.name").toLowerCase(Locale.ROOT);
        String name = System.getProperty("os.arch").toLowerCase(Locale.ROOT);
        String arch;
        if (name.equals("amd64") || name.equals("x86_64")) {
            arch = "x86_64";
        } else if (name.equals("aarch64") || name.equals("arm64")) {
            arch = "aarch64";
        } else {
            throw new MojoExecutionException(
                    "krusty publishes no compiler for the " + name + " architecture; set -Dkrusty.binary=<path>");
        }
        if (os.startsWith("linux")) return arch + "-unknown-linux-gnu";
        if (os.startsWith("mac")) return arch + "-apple-darwin";
        if (os.startsWith("windows")) return arch + "-pc-windows-msvc";
        throw new MojoExecutionException("krusty publishes no compiler for " + os + "; set -Dkrusty.binary=<path>");
    }

    private static String expectedSha256(String releases, String release, String archive)
            throws MojoExecutionException {
        String sums = new String(download(releases + "/v" + release + "/krusty-" + release + "-SHA256SUMS"),
                StandardCharsets.UTF_8);
        for (String line : sums.split("\n")) {
            String[] columns = line.trim().split("\\s+\\*?");
            if (columns.length == 2 && columns[1].equals(archive)) return columns[0];
        }
        throw new MojoExecutionException("the krusty " + release + " release lists no checksum for " + archive);
    }

    private static byte[] download(String url) throws MojoExecutionException {
        try {
            // Bounded, so a stalled server fails this build instead of holding the cache lock forever.
            URLConnection connection = URI.create(url).toURL().openConnection();
            connection.setConnectTimeout(30_000);
            connection.setReadTimeout(60_000);
            try (InputStream in = connection.getInputStream()) {
                return in.readAllBytes();
            }
        } catch (IOException e) {
            throw new MojoExecutionException(
                    "could not download " + url + "; set -Dkrusty.binary=<path> to use a local compiler", e);
        }
    }

    private static String sha256(byte[] bytes) {
        try {
            StringBuilder hex = new StringBuilder();
            for (byte b : MessageDigest.getInstance("SHA-256").digest(bytes)) hex.append(String.format("%02x", b));
            return hex.toString();
        } catch (NoSuchAlgorithmException e) {
            throw new IllegalStateException(e);
        }
    }

    private static InputStream zipEntry(byte[] bytes, String name) throws IOException {
        ZipInputStream zip = new ZipInputStream(new ByteArrayInputStream(bytes));
        for (ZipEntry entry = zip.getNextEntry(); entry != null; entry = zip.getNextEntry()) {
            if (entry.getName().equals(name)) return zip;
        }
        throw new IOException("the krusty archive has no " + name);
    }

    /** The one regular file {@code name} of a gzipped ustar archive, as {@code just package} writes it. */
    private static InputStream tarEntry(byte[] bytes, String name) throws IOException {
        byte[] tar;
        try (GZIPInputStream in = new GZIPInputStream(new ByteArrayInputStream(bytes));
             ByteArrayOutputStream out = new ByteArrayOutputStream()) {
            in.transferTo(out);
            tar = out.toByteArray();
        }
        int offset = 0;
        while (offset + 512 <= tar.length && tar[offset] != 0) {
            String entryName = new String(tar, offset, 100, StandardCharsets.US_ASCII).replace("\0", "");
            if (entryName.startsWith("./")) entryName = entryName.substring(2);
            int size = Integer.parseInt(
                    new String(tar, offset + 124, 12, StandardCharsets.US_ASCII).replace("\0", "").trim(), 8);
            char type = (char) tar[offset + 156];
            int data = offset + 512;
            if (entryName.equals(name) && (type == '0' || type == '\0')) {
                return new ByteArrayInputStream(tar, data, size);
            }
            offset = data + (size + 511) / 512 * 512;
        }
        throw new IOException("the krusty archive has no " + name);
    }
}
