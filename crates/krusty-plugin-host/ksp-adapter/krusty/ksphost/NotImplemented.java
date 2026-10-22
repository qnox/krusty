package krusty.ksphost;

// A KSP API member the host does not implement yet. It is reported to krusty by name, which then
// runs the module's processors on KSP itself rather than answer with something partial.
final class NotImplemented extends UnsupportedOperationException {
    final String member;

    NotImplemented(String member) {
        super("krusty's KSP host does not implement " + member + " yet");
        this.member = member;
    }

    // The first NotImplemented in `error`'s causes, which a processor may have wrapped.
    static NotImplemented in(Throwable error) {
        for (Throwable cause = error; cause != null; cause = cause.getCause()) {
            if (cause instanceof NotImplemented) {
                return (NotImplemented) cause;
            }
        }
        return null;
    }
}
