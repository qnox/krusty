"""The krusty compiler as a Bazel toolchain.

A released `@krusty` module registers one `krusty_toolchain` per host the compiler is published for
(see `extensions.bzl`), so Bazel picks the binary matching the execution platform and fetches only
that archive. `--@krusty//:krusty_binary=<label>` still overrides it with any executable target.
"""

KrustyCompilerFromToolchainInfo = provider(
    doc = "Marks the `krusty_binary` flag's default: take the compiler from the registered toolchain.",
    fields = {},
)

def _krusty_toolchain_impl(ctx):
    return [platform_common.ToolchainInfo(compiler = ctx.attr.compiler[DefaultInfo].files_to_run)]

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
    },
)

def _compiler_from_toolchain_impl(_ctx):
    return [KrustyCompilerFromToolchainInfo()]

compiler_from_toolchain = rule(
    doc = "The `krusty_binary` flag's default, standing for the compiler of the resolved toolchain.",
    implementation = _compiler_from_toolchain_impl,
)

def krusty_compiler(ctx):
    """The compiler a rule runs: the `krusty_binary` flag's target, else the resolved toolchain's.

    Args:
      ctx: the rule context; the rule has a `_krusty` attribute holding the flag and requests
        `@krusty//:toolchain_type` as an optional toolchain.

    Returns:
      The compiler's `FilesToRunProvider`.
    """
    flag = ctx.attr._krusty
    if KrustyCompilerFromToolchainInfo not in flag:
        compiler = flag[DefaultInfo].files_to_run
        if compiler.executable == None:
            fail("--@krusty//:krusty_binary must name an executable target, not %s" % flag.label)
        return compiler
    toolchain = ctx.toolchains[Label("//:toolchain_type")]
    if toolchain == None:
        fail("no krusty compiler for this execution platform: this @krusty module has no release " +
             "for it (a source checkout has none at all); set --@krusty//:krusty_binary to a " +
             "krusty executable target")
    return toolchain.compiler
