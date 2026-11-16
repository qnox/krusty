//! Resolver warnings stay off the tooling protocol stream.

use super::common;

#[test]
fn a_resolver_warning_stays_off_the_protocol_stream() {
    let root = common::scratch_dir().expect("scratch dir");
    let outdir = root.join("classes");
    std::fs::create_dir_all(&outdir).expect("classes dir");
    let java = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/krusty-lsp/java/krusty/tooling");
    let sources = std::env::join_paths([
        java.join("Protocol.java"),
        java.join("ProtocolStreamTest.java"),
    ])
    .expect("source paths")
    .into_string()
    .expect("utf-8 source paths");
    let outdir = outdir.to_str().expect("utf-8 outdir");
    let out = common::javac_run(
        &sources,
        outdir,
        outdir,
        "krusty.tooling.ProtocolStreamTest",
    )
    .expect("pooled JavaRunner unavailable");
    assert_eq!(out.trim(), "ok", "{out}");
}
