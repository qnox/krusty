"""Build a JVM library from Kotlin sources with krusty.

`krusty_jvm_library` is a drop-in-shaped alternative to a `kt_jvm_library`/`jvm_library` target: it
compiles `srcs` with the krusty CLI and returns a `JavaInfo`, so ordinary `java_*`/`kt_*` rules can
depend on it without knowing which compiler produced the jar.

    load("@krusty//bazel:defs.bzl", "krusty_jvm_library")

    krusty_jvm_library(
        name = "util",
        srcs = glob(["src/**/*.kt"]),
        deps = ["//platform/core-api"],
        module_name = "intellij.platform.util",
        kotlinc_opts = ["-Xjvm-default=all", "-progressive"],
        jvm_target = "25",
    )

The rule deliberately speaks kotlinc's own command line rather than inventing one: krusty accepts the
same flags, so a target can carry the options the project already uses (`build/compiler-options.bzl`
in intellij-community) unchanged.

Arguments go through a Bazel param file in `multiline` format, referenced as `@file` — the spelling
krusty's CLI reads. Long classpaths and thousands of sources therefore never hit the OS argument
limit.

NOT a persistent worker yet. Every action starts a fresh krusty process, which is affordable because
krusty is a native binary with no JVM to warm up, but it does not yet match the worker protocol of
intellij-community's own `jvm-inc-builder` (`--flagfile=`, `--out`/`--abi-out`/`--kotlin-cri-out`,
`--srcs`/`--cp`/`--friends`, `supports-multiplex-workers`). Two things must land before that form is
useful: krusty has no Java front end, so a target mixing `.java` and `.kt` sources cannot be built by
this rule at all, and no ABI jar is produced (`compile_jar` is the full jar).
"""

load("@rules_java//java:defs.bzl", "JavaInfo")

def _krusty_jvm_library_impl(ctx):
    output_jar = ctx.actions.declare_file(ctx.label.name + ".jar")

    # Both `deps` and `exports` are visible while compiling this target. `exports` additionally
    # propagates to consumers through the JavaInfo below; omitting it here makes a source that
    # imports an exported library fail even though ordinary java_library accepts that shape.
    dep_infos = [dep[JavaInfo] for dep in ctx.attr.deps if JavaInfo in dep]
    export_infos = [dep[JavaInfo] for dep in ctx.attr.exports if JavaInfo in dep]
    compile_jars = depset(transitive = [
        info.transitive_compile_time_jars
        for info in dep_infos + export_infos
    ])

    args = ctx.actions.args()
    args.set_param_file_format("multiline")
    args.use_param_file("@%s", use_always = True)

    args.add("-d", output_jar)
    args.add("-module-name", ctx.attr.module_name or ctx.label.name)
    if ctx.attr.jvm_target:
        args.add("-jvm-target", ctx.attr.jvm_target)

    # `-classpath` takes ONE argument, so the entries are joined here rather than added one by one.
    # `omit_if_empty` keeps a target with no dependencies from passing an empty `-classpath`.
    args.add_joined("-classpath", compile_jars, join_with = ":", omit_if_empty = True)

    # The project's own kotlinc flags, verbatim.
    args.add_all(ctx.attr.kotlinc_opts)

    # Sources last: krusty treats a non-flag argument as a source path.
    args.add_all(ctx.files.srcs)

    ctx.actions.run(
        mnemonic = "KrustyCompile",
        progress_message = "Compiling %%{label} with krusty (%d source(s))" % len(ctx.files.srcs),
        executable = ctx.executable._krusty,
        arguments = [args],
        inputs = depset(ctx.files.srcs, transitive = [compile_jars]),
        outputs = [output_jar],
        env = ctx.attr.env,
    )

    return [
        DefaultInfo(files = depset([output_jar])),
        JavaInfo(
            output_jar = output_jar,
            # krusty emits no ABI jar yet, so the full jar serves as the compile jar. Downstream
            # targets therefore recompile on any change, not only on ABI changes.
            compile_jar = output_jar,
            deps = dep_infos,
            exports = export_infos,
            neverlink = ctx.attr.neverlink,
        ),
    ]

krusty_jvm_library = rule(
    doc = "Compiles Kotlin sources with krusty into a jar, exposed as a JavaInfo.",
    implementation = _krusty_jvm_library_impl,
    attrs = {
        "srcs": attr.label_list(
            doc = "Kotlin sources. krusty has no Java front end, so `.java` sources are rejected.",
            allow_files = [".kt"],
            mandatory = True,
        ),
        "deps": attr.label_list(
            doc = "Targets whose compile jars form this target's classpath.",
            providers = [JavaInfo],
        ),
        "exports": attr.label_list(
            doc = "Dependencies re-exported to this target's consumers.",
            providers = [JavaInfo],
        ),
        "module_name": attr.string(
            doc = "kotlinc `-module-name`; the target name by default. Names the " +
                  "`META-INF/<name>.kotlin_module` index, through which a consumer discovers this " +
                  "module's top-level declarations — two targets sharing a name shadow each other.",
        ),
        "jvm_target": attr.string(
            doc = "kotlinc `-jvm-target` (e.g. \"25\"); krusty's default when unset.",
        ),
        "kotlinc_opts": attr.string_list(
            doc = "Additional kotlinc flags, passed through verbatim.",
        ),
        "neverlink": attr.bool(
            doc = "Provide this library for compilation only, never at runtime.",
            default = False,
        ),
        "env": attr.string_dict(
            doc = "Environment for the compile action (e.g. JAVA_HOME for the platform classpath).",
        ),
        "_krusty": attr.label(
            doc = "The krusty compiler binary, selected by `--@krusty//bazel:krusty_binary`.",
            default = Label("//bazel:krusty_binary"),
            executable = True,
            cfg = "exec",
        ),
    },
)
