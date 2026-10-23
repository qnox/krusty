"""The krusty compiler as a Bazel toolchain.

A released `@krusty` module registers one `krusty_toolchain` per host the compiler is published for
(see `extensions.bzl`), so Bazel picks the binary matching the execution platform and fetches only
that archive. `--@krusty//:krusty_binary=<label>` still overrides it with any executable target.
"""

load("@rules_java//java/common:java_common.bzl", "java_common")

KrustyCompilerFromToolchainInfo = provider(
    doc = "Marks the `krusty_binary` flag's default: take the compiler from the registered toolchain.",
    fields = {},
)

def _krusty_toolchain_impl(ctx):
    return [platform_common.ToolchainInfo(
        compiler = ctx.attr.compiler[DefaultInfo].files_to_run,
        classpath_separator = ctx.attr.classpath_separator,
        java_runtime = ctx.attr.java_runtime[java_common.JavaRuntimeInfo],
    )]

krusty_toolchain = rule(
    doc = "One prebuilt krusty compiler, offered to rules through `@krusty//:toolchain_type`.",
    implementation = _krusty_toolchain_impl,
    attrs = {
        "compiler": attr.label(
            doc = "The krusty executable.",
            mandatory = True,
            allow_single_file = True,
            executable = True,
            cfg = "exec",
        ),
        "classpath_separator": attr.string(
            doc = "What the compiler splits `-classpath` on: `;` on Windows, `:` elsewhere.",
            default = ":",
            values = [":", ";"],
        ),
        "java_runtime": attr.label(
            doc = "The Java runtime whose home becomes `JAVA_HOME`. An exec dependency of the " +
                  "toolchain, so it is built for the execution platform the compiler runs on.",
            default = Label("@rules_java//toolchains:current_java_runtime"),
            cfg = "exec",
            providers = [java_common.JavaRuntimeInfo],
        ),
    },
)

def _compiler_from_toolchain_impl(_ctx):
    return [KrustyCompilerFromToolchainInfo()]

compiler_from_toolchain = rule(
    doc = "The `krusty_binary` flag's default, standing for the compiler of the resolved toolchain.",
    implementation = _compiler_from_toolchain_impl,
)

_TOOLCHAIN_TYPE = Label("//:toolchain_type")

def krusty_compiler(ctx):
    """The compiler a rule runs: the `krusty_binary` flag's target, else the resolved toolchain's.

    Args:
      ctx: the rule context; the rule has a `_krusty` attribute holding the flag, an exec
        `_java_runtime`, and requests `@krusty//:toolchain_type` as an optional toolchain.

    Returns:
      A struct of the compiler's `FilesToRunProvider` (`executable`), the separator it splits
      `-classpath` on (`classpath_separator`), the `JavaRuntimeInfo` for its execution platform
      (`java_runtime`), and the toolchain type an action running it must name (`toolchain`, None
      for a flag's binary). A flag's binary runs in the rule's default execution group, beside the
      rule's own `_java_runtime`, and is taken to run on the host Bazel runs on, so it gets the
      host's separator. The toolchain's compiler runs on the platform its toolchain was resolved
      for, beside the toolchain's own Java runtime.
    """
    flag = ctx.attr._krusty
    if KrustyCompilerFromToolchainInfo not in flag:
        compiler = flag[DefaultInfo].files_to_run
        if compiler.executable == None:
            fail("--@krusty//:krusty_binary must name an executable target, not %s" % flag.label)
        return struct(
            executable = compiler,
            classpath_separator = ctx.configuration.host_path_separator,
            java_runtime = ctx.attr._java_runtime[java_common.JavaRuntimeInfo],
            toolchain = None,
        )
    toolchain = ctx.toolchains[_TOOLCHAIN_TYPE]
    if toolchain == None:
        fail("no krusty compiler for this execution platform: this @krusty module has no release " +
             "for it (a source checkout has none at all); set --@krusty//:krusty_binary to a " +
             "krusty executable target")
    return struct(
        executable = toolchain.compiler,
        classpath_separator = toolchain.classpath_separator,
        java_runtime = toolchain.java_runtime,
        toolchain = _TOOLCHAIN_TYPE,
    )
