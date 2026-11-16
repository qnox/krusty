package krusty.tooling;

/**
 * A resolver warning has to leave the protocol stream. The language server reads one JSON
 * response from stdout; a {@code [WARNING]} line there ends the resolve early.
 */
public final class ProtocolStreamTest {
    public static void main(String[] args) {
        Protocol.capture();
        System.out.println("[WARNING] The POM for example:example:jar:1.0 is invalid");
        Protocol.respond(0, "", "");
    }

    private ProtocolStreamTest() {}
}
