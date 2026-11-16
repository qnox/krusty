package krusty.tooling;

import java.io.ByteArrayOutputStream;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;

/**
 * A resolver warning has to leave the protocol stream. The driver captures both streams, runs the
 * warning through {@link Protocol#capture()}, and checks the protocol stream and the warning
 * before restoring them. The pooled runner only sees this driver's own {@code ok}.
 */
public final class ProtocolStreamTest {
    public static void main(String[] args) throws Exception {
        PrintStream savedOut = System.out;
        PrintStream savedErr = System.err;
        ByteArrayOutputStream outBuf = new ByteArrayOutputStream();
        ByteArrayOutputStream errBuf = new ByteArrayOutputStream();
        System.setOut(new PrintStream(outBuf, true, StandardCharsets.UTF_8));
        System.setErr(new PrintStream(errBuf, true, StandardCharsets.UTF_8));
        try {
            Protocol.capture();
            System.out.println("[WARNING] The POM for example:example:jar:1.0 is invalid");
            Protocol.respond(0, "", "");
        } finally {
            System.setOut(savedOut);
            System.setErr(savedErr);
        }
        String protocol = outBuf.toString(StandardCharsets.UTF_8).trim();
        String warnings = errBuf.toString(StandardCharsets.UTF_8);
        if (!protocol.equals("{\"status\":0,\"stdout\":\"\",\"stderr\":\"\"}")) {
            throw new AssertionError("protocol stream: " + protocol);
        }
        if (!warnings.contains("[WARNING] The POM for example:example:jar:1.0 is invalid")) {
            throw new AssertionError("stderr: " + warnings);
        }
        System.out.print("ok");
    }

    private ProtocolStreamTest() {}
}
