import java.nio.file.FileSystems;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.PathMatcher;
import java.util.regex.PatternSyntaxException;

/**
 * Judges how the Kotlin Toolchain treats a `project.yaml` module glob, for the cases in
 * `crates/krusty-toolchain/tests/cases/globs.tsv` (run and cached by `tests/support/oracle.rs`).
 *
 * Input: one case per line, `pattern<TAB>path<TAB>path...`. Output: per case, the pattern, the
 * toolchain's normalized pattern, and either the matcher's verdict per path (`+path`/`-path`) or the
 * `PatternSyntaxException` message with newlines escaped as `\n`. The normalization is the
 * toolchain's own (`normalize` in `frontend/project/globs.kt`), transcribed here.
 */
public class GlobOracle {
    public static void main(String[] args) throws Exception {
        for (String line : Files.readAllLines(Path.of(args[0]))) {
            String[] fields = line.split("\t", -1);
            String pattern = fields[0];
            String normalized = normalize(pattern);
            StringBuilder out = new StringBuilder(pattern).append('\t').append(normalized).append('\t');
            try {
                FileSystems.getDefault().getPathMatcher("glob:" + pattern);
                PathMatcher matcher = FileSystems.getDefault().getPathMatcher("glob:" + normalized);
                for (int i = 1; i < fields.length; i++) {
                    out.append(matcher.matches(Path.of(fields[i]).normalize()) ? '+' : '-').append(fields[i]);
                    if (i + 1 < fields.length) out.append('\t');
                }
            } catch (PatternSyntaxException e) {
                out.append("!").append(e.getMessage().replace("\\", "\\\\").replace("\n", "\\n"));
            }
            System.out.println(out);
        }
    }

    private static final java.util.regex.Pattern DOT =
        java.util.regex.Pattern.compile("(^|/)\\.(/\\.)*(/|$)");
    private static final java.util.regex.Pattern DOT_DOT =
        java.util.regex.Pattern.compile("(^|/)(?!\\.\\./)[^/]+/\\.\\.(/|$)");
    private static final java.util.regex.Pattern SLASHES = java.util.regex.Pattern.compile("/{2,}");

    private static String collapsed(java.util.regex.Matcher m) {
        StringBuilder sb = new StringBuilder();
        while (m.find()) {
            String v = m.group();
            m.appendReplacement(sb, v.startsWith("/") && v.endsWith("/") ? "/" : "");
        }
        m.appendTail(sb);
        return sb.toString();
    }

    static String normalize(String pattern) {
        String cleaned = SLASHES.matcher(pattern).replaceAll("/");
        cleaned = collapsed(DOT.matcher(cleaned));
        while (DOT_DOT.matcher(cleaned).find()) {
            cleaned = collapsed(DOT_DOT.matcher(cleaned));
        }
        if (!cleaned.equals("/") && cleaned.endsWith("/")) {
            cleaned = cleaned.substring(0, cleaned.length() - 1);
        }
        return cleaned;
    }
}
