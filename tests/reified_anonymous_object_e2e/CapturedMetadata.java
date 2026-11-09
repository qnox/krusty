import kotlin.Metadata;
import kotlin.Pair;
import org.jetbrains.kotlin.metadata.ProtoBuf;
import org.jetbrains.kotlin.metadata.deserialization.NameResolver;
import org.jetbrains.kotlin.metadata.jvm.deserialization.JvmProtoBufUtil;

public class CapturedMetadata {
    public static void main(String[] args) throws Exception {
        Class<?> copy = Class.forName("test.CapturedSuperclassKt$box$$inlined$lift$1");
        Metadata metadata = copy.getAnnotation(Metadata.class);
        Pair<?, ProtoBuf.Class> decoded = JvmProtoBufUtil.readClassDataFrom(metadata.d1(), metadata.d2());
        NameResolver names = (NameResolver) decoded.getFirst();
        ProtoBuf.Type parent = decoded.getSecond().getSupertype(0);
        ProtoBuf.Type argument = parent.getArgument(0).getType();
        System.out.print(decoded.getSecond().getSupertypeCount() + "|"
            + names.getQualifiedClassName(parent.getClassName()) + "|"
            + argument.hasTypeParameter() + "|" + argument.getTypeParameter());
    }
}
