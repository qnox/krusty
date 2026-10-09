package krusty.buildtools;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.regex.Matcher;
import java.util.regex.Pattern;
import org.jetbrains.kotlin.buildtools.api.CompilerMessageRenderer;
import org.jetbrains.kotlin.buildtools.api.CompilerMessageRenderer.Severity;
import org.jetbrains.kotlin.buildtools.api.CompilerMessageRenderer.SourceLocation;
import org.jetbrains.kotlin.buildtools.api.KotlinLogger;

/**
 * krusty's diagnostics, read from its output and handed to the caller's logger.
 *
 * krusty prints kotlinc's command-line format: {@code path:line:col: severity: message} for a
 * located diagnostic and {@code severity: message} for an unlocated one. Its own lines start with
 * {@code krusty: } ({@code krusty: error: } for an error) and its success line with {@code ok: }.
 * Any other line continues the previous message (multi-line messages), or is plain output when
 * nothing precedes it.
 */
final class CompilerOutput {
    private static final Pattern LOCATED =
        Pattern.compile("^(.+?):(\\d+):(\\d+): (error|warning|info): (.*)$");
    private static final Pattern UNLOCATED = Pattern.compile("^(error|warning|info): (.*)$");
    private static final String KRUSTY_ERROR = "krusty: error: ";
    private static final String KRUSTY = "krusty: ";
    private static final String SUCCESS = "ok: ";

    private final CompilerMessageRenderer renderer;
    private final List<Message> messages = new ArrayList<>();

    CompilerOutput(CompilerMessageRenderer renderer) {
        this.renderer = renderer;
    }

    void read(InputStream stream) throws IOException {
        try (BufferedReader reader = new BufferedReader(new InputStreamReader(stream, StandardCharsets.UTF_8))) {
            String line;
            while ((line = reader.readLine()) != null) {
                accept(line);
            }
        }
    }

    private void accept(String line) {
        Matcher located = LOCATED.matcher(line);
        if (located.matches()) {
            messages.add(new Message(
                severity(located.group(4)),
                new StringBuilder(located.group(5)),
                located.group(1),
                Integer.parseInt(located.group(2)),
                Integer.parseInt(located.group(3)),
                true));
            return;
        }
        Matcher unlocated = UNLOCATED.matcher(line);
        if (unlocated.matches()) {
            messages.add(diagnostic(severity(unlocated.group(1)), unlocated.group(2)));
            return;
        }
        if (line.startsWith(KRUSTY_ERROR)) {
            messages.add(diagnostic(Severity.ERROR, line.substring(KRUSTY_ERROR.length())));
            return;
        }
        if (line.startsWith(KRUSTY)) {
            messages.add(status(Severity.INFO, line));
            return;
        }
        if (line.startsWith(SUCCESS)) {
            messages.add(status(Severity.DEBUG, line));
            return;
        }
        if (messages.isEmpty()) {
            messages.add(status(Severity.INFO, line));
        } else {
            messages.get(messages.size() - 1).text.append('\n').append(line);
        }
    }

    private static Message diagnostic(Severity severity, String text) {
        return new Message(severity, new StringBuilder(text), null, 0, 0, true);
    }

    /** krusty's own progress output: logged as is, never shown to the caller as a diagnostic. */
    private static Message status(Severity severity, String text) {
        return new Message(severity, new StringBuilder(text), null, 0, 0, false);
    }

    boolean hasErrors() {
        return messages.stream().anyMatch(message -> message.severity == Severity.ERROR);
    }

    void report(KotlinLogger logger) {
        for (Message message : messages) {
            SourceLocation location = message.location();
            String text = renderer != null && message.diagnostic
                ? renderer.render(message.severity, message.text.toString(), location)
                : plain(message, location);
            switch (message.severity) {
                case ERROR -> logger.error(text, null);
                case WARNING -> logger.warn(text, null);
                case INFO -> logger.info(text);
                case DEBUG -> logger.debug(text);
            }
        }
    }

    private static String plain(Message message, SourceLocation location) {
        if (location == null) {
            return message.text.toString();
        }
        return location.getPath() + ":" + location.getLine() + ":" + location.getColumn() + ": " + message.text;
    }

    private static Severity severity(String text) {
        return switch (text) {
            case "error" -> Severity.ERROR;
            case "warning" -> Severity.WARNING;
            default -> Severity.INFO;
        };
    }

    private record Message(
        Severity severity,
        StringBuilder text,
        String path,
        int line,
        int column,
        boolean diagnostic
    ) {
        SourceLocation location() {
            if (path == null) {
                return null;
            }
            return new SourceLocation(path, line, column, line, column, lineContent(path, line));
        }
    }

    private static String lineContent(String path, int line) {
        try (var lines = Files.lines(Path.of(path), StandardCharsets.UTF_8)) {
            return lines.skip(line - 1L).findFirst().orElse(null);
        } catch (IOException | java.io.UncheckedIOException e) {
            return null;
        }
    }
}
