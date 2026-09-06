# Pre-push-only task caching. Explicit `just test` runs remain uncached.
set shell := ["bash", "-uc"]
set unstable
set lists

tracked := split(`git ls-files`)
toolchain := `rustc -vV && cargo -V && just --version && java -version 2>&1`
environment := `env | LC_ALL=C sort | sed -nE '/^(CARGO_BUILD_TARGET|CARGO_INCREMENTAL|JAVA_HOME|KRUSTY_[^=]*|MACOSX_DEPLOYMENT_TARGET|PATH|RUSTFLAGS|RUSTUP_TOOLCHAIN)=/p'`

[cache(extra=toolchain + environment, inputs=tracked)]
[script]
test:
    ./run-tests.sh
