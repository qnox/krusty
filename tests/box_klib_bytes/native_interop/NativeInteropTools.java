import java.nio.charset.StandardCharsets;
import java.util.HexFormat;
import org.jetbrains.kotlin.konan.target.Platform;
import org.jetbrains.kotlin.konan.target.PlatformManager;
import org.jetbrains.kotlin.konan.target.KonanTarget;
import org.jetbrains.kotlin.konan.target.ClangArgs;

public class NativeInteropTools {
    private static void emit(String kind, String value) {
        System.out.println(kind + ":" + HexFormat.of().formatHex(value.getBytes(StandardCharsets.UTF_8)));
    }
    public static void main(String[] args) {
        String root = new String(HexFormat.of().parseHex("@ROOT_HEX@"), StandardCharsets.UTF_8);
        String name = "@TARGET@";
        PlatformManager manager = new PlatformManager(root, false, null);
        KonanTarget target = manager.getTargets().get(name);
        if (target == null) throw new IllegalArgumentException("Unknown Native target: " + name);
        Platform platform = manager.platform(target);
        platform.downloadDependencies();
        ClangArgs.Native clang = platform.getClang();
        System.out.println("NATIVE_CLANG_TOOLS");
        emit("clang", platform.getAbsoluteLlvmHome() + "/bin/clang");
        emit("clangxx", platform.getAbsoluteLlvmHome() + "/bin/clang++");
        emit("ar", clang.llvmAr().get(0));
        emit("input", platform.getAbsoluteLlvmHome());
        emit("input", platform.getAbsoluteTargetToolchain());
        emit("input", platform.getAbsoluteTargetSysRoot());
        emit("input", platform.absolute(platform.getLibffiDir()));
        for (String argument : clang.getClangArgs()) emit("c", argument);
        for (String argument : clang.getClangXXArgs()) emit("cpp", argument);
    }
}
