"""The `krusty` module extension: the prebuilt compiler of one krusty release, per host.

The release's own `MODULE.bazel` carries a `krusty.release(...)` tag, stamped by the release job
with the version and the SHA-256 of every compiler archive it published. The extension declares
one archive repository per host and a hub, `@krusty_toolchains`, whose `toolchain` targets select
among them by execution platform; Bazel downloads only the archive a build actually uses.

A root module may declare its own `krusty.release(...)` to run another release's compiler with
these rules, or to fetch the archives from a mirror. A source checkout of krusty has no tag, so it
registers no toolchain and its rules need `--@krusty//:krusty_binary`.
"""

load("@bazel_tools//tools/build_defs/repo:http.bzl", "http_archive")

# Every host the release job publishes a compiler for, with the constraints it runs under.
HOSTS = {
    "aarch64-apple-darwin": ["@platforms//os:macos", "@platforms//cpu:aarch64"],
    "aarch64-pc-windows-msvc": ["@platforms//os:windows", "@platforms//cpu:aarch64"],
    "aarch64-unknown-linux-gnu": ["@platforms//os:linux", "@platforms//cpu:aarch64"],
    "x86_64-apple-darwin": ["@platforms//os:macos", "@platforms//cpu:x86_64"],
    "x86_64-pc-windows-msvc": ["@platforms//os:windows", "@platforms//cpu:x86_64"],
    "x86_64-unknown-linux-gnu": ["@platforms//os:linux", "@platforms//cpu:x86_64"],
}

_RELEASES = "https://github.com/qnox/krusty/releases/download/v{version}/{archive}"

_release = tag_class(
    doc = "The krusty release whose compilers the toolchains run.",
    attrs = {
        "version": attr.string(doc = "The release version, as in its tag without the `v`.", mandatory = True),
        "sha256": attr.string_dict(
            doc = "Compiler archive SHA-256 by host triple. Hosts left out get no toolchain.",
            mandatory = True,
        ),
        "url": attr.string(
            doc = "Where an archive is downloaded from; `{version}` and `{archive}` are substituted.",
            default = _RELEASES,
        ),
    },
)

def _toolchains_hub_impl(rctx):
    toolchains = []
    for triple in rctx.attr.hosts:
        toolchains.append("""toolchain(
    name = "{triple}",
    exec_compatible_with = {constraints},
    toolchain = "@krusty_{triple}//:toolchain",
    toolchain_type = "{toolchain_type}",
)
""".format(
            triple = triple,
            constraints = repr(HOSTS[triple]),
            toolchain_type = str(Label("//:toolchain_type")),
        ))
    rctx.file("BUILD.bazel", "\n".join(toolchains))

_toolchains_hub = repository_rule(
    implementation = _toolchains_hub_impl,
    attrs = {"hosts": attr.string_list()},
)

def _selected_release(module_ctx):
    """The root module's `release` tag, else the krusty module's own, else None."""
    for module in module_ctx.modules:
        if not module.is_root and module.name != "krusty":
            continue
        if len(module.tags.release) > 1:
            fail("module %s declares krusty.release more than once" % module.name)
        if module.tags.release:
            return module.tags.release[0]
    return None

def _krusty_impl(module_ctx):
    release = _selected_release(module_ctx)
    hosts = []
    if release:
        for triple, sha256 in sorted(release.sha256.items()):
            if triple not in HOSTS:
                fail("krusty.release names unknown host %s; known: %s" % (triple, ", ".join(sorted(HOSTS))))
            executable = "krusty.exe" if "windows" in triple else "krusty"
            archive = "krusty-{}-{}.{}".format(release.version, triple, "zip" if "windows" in triple else "tar.gz")
            http_archive(
                name = "krusty_" + triple,
                urls = [release.url.format(version = release.version, archive = archive)],
                sha256 = sha256,
                build_file_content = """load("{toolchain_bzl}", "krusty_toolchain")

krusty_toolchain(
    name = "toolchain",
    compiler = "{executable}",
    classpath_separator = "{separator}",
    visibility = ["//visibility:public"],
)
""".format(
                    toolchain_bzl = str(Label("//:toolchain.bzl")),
                    executable = executable,
                    separator = ";" if "windows" in triple else ":",
                ),
            )
            hosts.append(triple)
    _toolchains_hub(name = "krusty_toolchains", hosts = hosts)
    return module_ctx.extension_metadata(reproducible = True)

krusty = module_extension(
    doc = "Declares the prebuilt compiler of a krusty release as one toolchain per host.",
    implementation = _krusty_impl,
    tag_classes = {"release": _release},
)
