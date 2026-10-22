package krusty.ksphost;

import com.google.devtools.ksp.symbol.FileLocation;
import com.google.devtools.ksp.symbol.KSDeclaration;
import java.util.Collections;
import java.util.Iterator;
import java.util.List;
import kotlin.sequences.Sequence;
import kotlin.sequences.SequencesKt;

// What every node of the host's symbol model shares.
final class Nodes {
    private Nodes() {}

    static <T> Sequence<T> sequence(List<T> list) {
        return SequencesKt.asSequence(list.iterator());
    }

    static <T> Sequence<T> empty() {
        Iterator<T> none = Collections.emptyIterator();
        return SequencesKt.asSequence(none);
    }

    static NotImplemented notImplemented(String member) {
        return new NotImplemented(member);
    }

    static FileLocation location(SourceFile file, int line) {
        return new FileLocation(file.getFilePath(), line);
    }

    static Sequence<KSDeclaration> declarations(
            String owner, boolean complete, List<KSDeclaration> declarations) {
        if (!complete) {
            throw notImplemented(owner + ".declarations of a body with functions or properties");
        }
        return sequence(declarations);
    }
}
