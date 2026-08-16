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

Set `use_worker = True` to run krusty as a Bazel PERSISTENT WORKER instead of one process per
action. The worker speaks the argument surface of intellij-community's own `jvm-inc-builder`
(`--out`/`--abi-out`/`--kotlin-cri-out`, `--srcs`/`--cp`/`--friends`, `--jvm_default`,
`--x_no_param_assertions`, …), so a `jvm_library` target's existing options carry over unchanged, and
it keeps its decoded classpath warm across requests.

Two limits hold in both modes. krusty has no Java front end, so a target mixing `.java` and `.kt`
sources cannot be built by this rule at all — the worker REFUSES such a request rather than emitting
a jar missing those classes. And no reduced ABI jar is produced: `--abi-out` receives a copy of the
full jar, so a dependent rebuilds on any change rather than only on an ABI change.
"""

load("@rules_java//java:defs.bzl", "JavaInfo")

def _worker_arguments(ctx, output_jar, abi_jar, compile_jars):
    """The `jvm-inc-builder` vocabulary, which krusty's `--persistent_worker` mode parses.

    Deliberately NOT krusty's own command line: speaking the builder's surface is what lets a target
    move between the two compilers without rewriting its options.
    """
    args = ctx.actions.args()
    args.set_param_file_format("multiline")
    args.use_param_file("--flagfile=%s", use_always = True)

    args.add("--target_label", ctx.label)
    args.add("--kotlin_module_name", ctx.attr.module_name or ctx.label.name)
    args.add("--out", output_jar)
    args.add("--abi-out", abi_jar)
    if ctx.attr.jvm_target:
        args.add("--jvm_target", ctx.attr.jvm_target)

    # Every Kotlin source is a `.kt`: `srcs` rejects anything else, so the worker's Java refusal
    # cannot trigger from this rule — it guards the surface, not this caller.
    args.add_all("--srcs", ctx.files.srcs)
    args.add_all("--cp", compile_jars)

    # `--java-count 0` is stated rather than omitted: it is how the builder says "no Java in this
    # action", and saying it explicitly documents that this rule never has any.
    args.add("--java-count", "0")
    return args

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

    outputs = [output_jar]
    if ctx.attr.use_worker:
        # A worker's outputs are DECLARED, so every one must be written or bazel fails the action —
        # which is why krusty writes an abi jar and a cri file even though it has nothing distinct to
        # put in them.
        abi_jar = ctx.actions.declare_file(ctx.label.name + ".abi.jar")
        outputs.append(abi_jar)
        ctx.actions.run(
            mnemonic = "KrustyCompile",
            progress_message = "Compiling %%{label} with krusty worker (%d source(s))" % len(ctx.files.srcs),
            executable = ctx.executable._krusty,
            arguments = [_worker_arguments(ctx, output_jar, abi_jar, compile_jars)],
            inputs = depset(ctx.files.srcs, transitive = [compile_jars]),
            outputs = outputs,
            env = ctx.attr.env,
            execution_requirements = {
                "supports-workers": "1",
                # krusty's worker speaks line-delimited JSON, not the protobuf protocol.
                "requires-worker-protocol": "json",
            },
        )
    else:
        ctx.actions.run(
            mnemonic = "KrustyCompile",
            progress_message = "Compiling %%{label} with krusty (%d source(s))" % len(ctx.files.srcs),
            executable = ctx.executable._krusty,
            arguments = [args],
            inputs = depset(ctx.files.srcs, transitive = [compile_jars]),
            outputs = outputs,
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
        "use_worker": attr.bool(
            doc = "Run krusty as a Bazel persistent worker, speaking jvm-inc-builder's argument " +
                  "surface, instead of one process per action.",
            default = False,
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
