# The test executables of krusty's own suite, from Cargo's `--message-format=json` artifact stream.
# Shared by the plain CI run (scripts/own-tests.sh) and instrumented coverage (scripts/coverage.sh),
# so both select the same binaries. krusty-lsp, krusty-toolchain and krusty-download run every test target; the
# backend and CLI crates run their library unit tests; the root package runs its library unit tests
# and the product `e2e` target. The external-corpus `conformance` target is never selected.
def package_is($name):
  test("(^|[/# ])" + $name + "([#@ ]|$)");
select(.reason == "compiler-artifact" and .profile.test == true and .executable != null)
| . as $artifact
| ($artifact.package_id | tostring) as $package
| ($artifact.target.kind | join(",")) as $kind
| if ($package | package_is("krusty-lsp")) or ($package | package_is("krusty-toolchain"))
      or ($package | package_is("krusty-download")) then
    $artifact.executable
  elif (($package | package_is("krusty-cli")) or ($package | package_is("krusty-jvm"))
      or ($package | package_is("krusty-native"))
      or ($package | package_is("krusty-wasm")))
      and $kind == "lib" then
    $artifact.executable
  elif ($package | package_is("krusty"))
      and ($kind == "lib" or ($kind == "test" and $artifact.target.name == "e2e")) then
    $artifact.executable
  else
    empty
  end
