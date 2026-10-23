package krusty.ksphost;

import com.google.devtools.ksp.processing.KSPLogger;
import com.google.devtools.ksp.symbol.FileLocation;
import com.google.devtools.ksp.symbol.KSNode;
import com.google.devtools.ksp.symbol.Location;
import java.util.ArrayList;
import java.util.List;

// KSP's command-line logger: each message at or above the configured level becomes one line, as KSP
// prints it, which krusty passes on. An error, or a warning when warnings are errors, fails the run.
final class Logger implements KSPLogger {
    static final int LOGGING = 0;
    static final int INFO = 1;
    static final int WARN = 3;
    static final int ERROR = 5;

    private final int level;
    private final boolean warningsAsErrors;
    private final List<String> lines = new ArrayList<>();
    private boolean hasError;

    Logger(int level, boolean warningsAsErrors) {
        this.level = level;
        this.warningsAsErrors = warningsAsErrors;
    }

    boolean hasError() {
        return hasError;
    }

    // The lines logged since the last call.
    List<String> drain() {
        List<String> drained = new ArrayList<>(lines);
        lines.clear();
        return drained;
    }

    private static String decorate(String message, KSNode node) {
        Location location = node == null ? null : node.getLocation();
        if (location instanceof FileLocation) {
            FileLocation file = (FileLocation) location;
            return file.getFilePath() + ":" + file.getLineNumber() + ": " + message;
        }
        return message;
    }

    @Override
    public void logging(String message, KSNode symbol) {
        if (level <= LOGGING) {
            lines.add("v: [ksp] " + decorate(message, symbol));
        }
    }

    @Override
    public void info(String message, KSNode symbol) {
        if (level <= INFO) {
            lines.add("i: [ksp] " + decorate(message, symbol));
        }
    }

    @Override
    public void warn(String message, KSNode symbol) {
        if (warningsAsErrors) {
            error(message, symbol);
        } else if (level <= WARN) {
            lines.add("w: [ksp] " + decorate(message, symbol));
        }
    }

    @Override
    public void error(String message, KSNode symbol) {
        hasError = true;
        if (level <= ERROR) {
            lines.add("e: [ksp] " + decorate(message, symbol));
        }
    }

    @Override
    public void exception(Throwable error) {
        hasError = true;
        if (level <= ERROR) {
            lines.add("e: [ksp] " + error);
        }
    }
}
