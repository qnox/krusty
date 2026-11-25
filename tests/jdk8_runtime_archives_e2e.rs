//! A JDK 8 home keeps part of its runtime outside `rt.jar`: `jre/lib/jce.jar` and the jars in
//! `jre/lib/ext`. kotlinc reads them all as the JDK's classes, so a program naming a class from
//! each compiles, and krusty must emit the same classes.

use std::path::{Path, PathBuf};

use super::common;

/// `Cipher` is in `jre/lib/jce.jar` and `ZipFileSystemProvider` in `jre/lib/ext/zipfs.jar`.
const SOURCE: &str = "import com.sun.nio.zipfs.ZipFileSystemProvider
import javax.crypto.Cipher

fun algorithm(cipher: Cipher): String = cipher.algorithm

fun scheme(provider: ZipFileSystemProvider): String = provider.scheme

fun box(): String = \"OK\"
";

fn jdk8_home() -> PathBuf {
    ["JAVA_HOME_8_X64", "JAVA_HOME_8_ARM64", "KRUSTY_JDK_8_HOME"]
        .iter()
        .filter_map(|name| std::env::var_os(name).map(PathBuf::from))
        .find(|home| home.join("jre/lib/rt.jar").is_file())
        .expect("a JDK 8 home is required: set JAVA_HOME_8_X64 or KRUSTY_JDK_8_HOME")
}

fn classes(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut found = std::fs::read_dir(dir)
        .expect("read compiler output")
        .map(|entry| entry.expect("compiler output entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "class")
        })
        .map(|path| {
            let name = path
                .file_name()
                .expect("class file name")
                .to_string_lossy()
                .into_owned();
            (name, std::fs::read(&path).expect("read class"))
        })
        .collect::<Vec<_>>();
    found.sort();
    found
}

#[test]
fn classes_of_the_other_runtime_archives_of_a_jdk_8_home_compile_as_kotlinc_does() {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let source = work.join("Box.kt");
    std::fs::write(&source, SOURCE).expect("write the fixture");
    let jdk = jdk8_home();
    let arguments = |output: &str| {
        vec![
            source.to_string_lossy().into_owned(),
            "-jdk-home".to_string(),
            jdk.to_string_lossy().into_owned(),
            "-d".to_string(),
            work.join(output).to_string_lossy().into_owned(),
        ]
    };

    let (code, stderr) = common::kotlinc_compile(&arguments("kotlinc")).expect("kotlinc");
    assert_eq!((code, stderr.as_str()), (0, ""), "kotlinc");

    let result = std::process::Command::new(common::krusty_binary())
        .args(arguments("krusty"))
        .output()
        .expect("run krusty");
    assert_eq!(
        (
            result.status.code(),
            String::from_utf8_lossy(&result.stderr).as_ref()
        ),
        (Some(0), ""),
        "krusty"
    );

    let expected = classes(&work.join("kotlinc"));
    assert_eq!(expected.len(), 1, "kotlinc classes");
    assert_eq!(classes(&work.join("krusty")), expected);
    let _ = std::fs::remove_dir_all(&work);
}
