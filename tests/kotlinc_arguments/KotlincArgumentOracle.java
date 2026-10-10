import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

import org.jetbrains.kotlin.cli.common.ArgumentsKt;
import org.jetbrains.kotlin.cli.common.arguments.ArgumentField;
import org.jetbrains.kotlin.cli.common.arguments.K2JVMCompilerArguments;
import org.jetbrains.kotlin.cli.common.arguments.ParseCommandLineArgumentsKt;
import org.jetbrains.kotlin.cli.common.messages.CompilerMessageSeverity;
import org.jetbrains.kotlin.cli.common.messages.CompilerMessageSourceLocation;
import org.jetbrains.kotlin.cli.common.messages.MessageCollector;
import org.jetbrains.kotlin.config.KotlinCompilerVersion;

/**
 * Runs kotlinc's own command-line parser over every case in a file and prints what it parsed and
 * reported. Cases are separated by an empty line; each line of a case is one argument. Only the
 * parser runs: no compiler environment is created.
 */
public class KotlincArgumentOracle {
    public static void main(String[] ignored) throws Exception {
        PrintStream out = new PrintStream(System.out, true, "UTF-8");
        out.println("version\t" + KotlinCompilerVersion.VERSION);
        String text = new String(Files.readAllBytes(Paths.get(CASES)), StandardCharsets.UTF_8);
        for (String block : text.split("\n\n", -1)) {
            if (block.isEmpty()) continue;
            List<String> arguments = new ArrayList<>();
            for (String line : block.split("\n", -1)) arguments.add(line);
            K2JVMCompilerArguments parsed = new K2JVMCompilerArguments();
            ParseCommandLineArgumentsKt.parseCommandLineArguments(arguments, parsed, false);
            out.println("case");
            for (String error : ParseCommandLineArgumentsKt.validateArgumentsAllErrors(parsed.getErrors())) {
                out.println("error\t" + escape(error));
            }
            List<String> warnings = new ArrayList<>();
            ArgumentsKt.reportArgumentParseProblems(new MessageCollector() {
                public void clear() {}
                public boolean hasErrors() { return false; }
                public void report(CompilerMessageSeverity severity, String message, CompilerMessageSourceLocation location) {
                    warnings.add(severity.getPresentableName() + "\t" + escape(message));
                }
            }, parsed);
            for (String warning : warnings) out.println(warning);
            for (Map.Entry<ArgumentField, List<Object>> entry : parsed.getExplicitArguments().entrySet()) {
                StringBuilder line = new StringBuilder("explicit\t").append(entry.getKey().getArgument().value());
                for (Object value : entry.getValue()) {
                    line.append('\t');
                    if (value instanceof List) {
                        line.append("[").append(String.join("|", (List<String>) value)).append("]");
                    } else {
                        line.append(escape(String.valueOf(value)));
                    }
                }
                out.println(line);
            }
            for (String free : parsed.getFreeArgs()) out.println("free\t" + escape(free));
        }
    }

    private static String escape(String text) {
        return text.replace("\\", "\\\\").replace("\n", "\\n").replace("\t", "\\t");
    }

    private static final String CASES = "@CASES@";
}
