use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::common;

const LIB: &str = "package lib\n\
                   internal class Hidden(val value: Int)\n\
                   class Visible(val value: Int)\n\
                   open class Parent {\n\
                       protected class ProtectedBox(val value: Int)\n\
                   }\n";

struct Fixture {
    _roots: Vec<FixtureRoot>,
    classpath: Vec<PathBuf>,
    jdk: Option<PathBuf>,
}

struct FixtureRoot(PathBuf);

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Fixture {
    fn new() -> Option<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);

        let stdlib = common::stdlib_jar();
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "krusty_internal_classpath_access_{epoch}_{}",
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).ok()?;
        let root = FixtureRoot(root);
        let output = root.0.join("classes");
        std::fs::create_dir(&output).ok()?;
        let source = root.0.join("Library.kt");
        std::fs::write(&source, LIB).ok()?;
        let args = vec![
            "-d".to_string(),
            output.to_string_lossy().into_owned(),
            "-cp".to_string(),
            stdlib.to_string_lossy().into_owned(),
            source.to_string_lossy().into_owned(),
        ];
        match common::kotlinc_compile(&args) {
            Some((0, _)) => {}
            Some((code, stderr)) => panic!("kotlinc fixture failed ({code}): {stderr}"),
            None => return None,
        }
        let java_sources = [(
            "PackageBox.java".to_string(),
            "package javafixture;\n\
             class PackageBox {\n\
                 PackageBox(int value) {}\n\
             }\n"
            .to_string(),
        )];
        let (java_output, _) = common::javac_compile(&java_sources, &[])?;
        let java_root = java_output.parent()?.to_path_buf();
        let jdk = Some({
            let home = common::java_home();
            PathBuf::from(format!("{home}/lib/modules"))
        });
        Some(Fixture {
            _roots: vec![root, FixtureRoot(java_root)],
            classpath: vec![stdlib, output, java_output],
            jdk,
        })
    }

    fn diagnostics(&self, source: &str) -> Vec<String> {
        common::front_end_diagnostics(source, &self.classpath, self.jdk.as_deref())
    }

    fn diagnostics_files(&self, sources: &[&str]) -> Vec<String> {
        common::front_end_diagnostics_files(sources, &self.classpath, self.jdk.as_deref())
    }
}

fn assert_no_underlying_name(diagnostics: &[String], underlying: &str) {
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.contains(underlying)),
        "underlying classifier name leaked: {diagnostics:?}"
    );
}

#[test]
fn classifier_access_diagnostics_follow_resolution_scope() {
    let Some(fixture) = Fixture::new() else {
        return;
    };

    assert_eq!(
        fixture.diagnostics(
            "import lib.Hidden\n\
             fun use(): Int { Hidden(1); return 0 }\n"
        ),
        ["cannot access 'Hidden': it is internal"]
    );
    assert_eq!(
        fixture.diagnostics(
            "import lib.Visible\n\
             fun use(): Int = Visible(1).value\n"
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        fixture.diagnostics("fun use(): Int { Hidden(1); return 0 }\n"),
        ["unresolved reference 'Hidden'."]
    );
    assert_eq!(
        fixture.diagnostics(
            "import lib.Hidden\n\
             fun use(Hidden: Int): Int { Hidden(); return 0 }\n"
        ),
        [
            "expression 'Hidden' of type 'Int' cannot be invoked as a function. \
          Function 'invoke()' is not found."
        ]
    );
    assert_eq!(
        fixture.diagnostics(
            "package consumer\n\
             import lib.Hidden\n\
             val Hidden: Int = 1\n\
             fun use(): Int { Hidden(); return 0 }\n"
        ),
        [
            "expression 'Hidden' of type 'Int' cannot be invoked as a function. \
          Function 'invoke()' is not found."
        ]
    );
    assert_eq!(
        fixture.diagnostics_files(&[
            "package lib\nclass Hidden(val value: Int)\n",
            "package lib\n\
             fun use(): Int = Hidden(1).value\n",
        ]),
        Vec::<String>::new()
    );
    assert_eq!(
        fixture.diagnostics(
            "package consumer\n\
             import lib.Parent\n\
             class Child : Parent() {\n\
                 fun use(): Int = ProtectedBox(1).value\n\
             }\n"
        ),
        Vec::<String>::new()
    );

    let alias_diagnostics = fixture.diagnostics_files(&[
        "package first\n\
         import lib.Hidden as FirstAlias\n\
         fun use(): Int { FirstAlias(1); return 0 }\n",
        "package second\n\
         import lib.Hidden as SecondAlias\n\
         fun use(): Int { SecondAlias(2); return 0 }\n",
    ]);
    assert_eq!(
        alias_diagnostics,
        [
            "cannot access 'FirstAlias': it is internal",
            "cannot access 'SecondAlias': it is internal",
        ]
    );
    assert_no_underlying_name(&alias_diagnostics, "Hidden");

    let protected_alias_diagnostics = fixture.diagnostics(
        "package consumer\n\
         import lib.Parent.ProtectedBox as Guard\n\
         fun use(): Int { Guard(1); return 0 }\n",
    );
    assert_eq!(
        protected_alias_diagnostics,
        ["cannot access 'Guard': it is protected"]
    );
    assert_no_underlying_name(&protected_alias_diagnostics, "ProtectedBox");

    assert_eq!(
        fixture.diagnostics(
            "package consumer\n\
             import javafixture.PackageBox\n\
             fun use(): Int { PackageBox(1); return 0 }\n",
        ),
        ["cannot access 'PackageBox': it is package-private"]
    );
}

#[test]
fn classifier_access_diagnostics_cover_type_positions() {
    let Some(fixture) = Fixture::new() else {
        return;
    };

    assert_eq!(
        fixture.diagnostics(
            "import lib.Hidden\n\
             fun use(value: Hidden): Int = 0\n"
        ),
        ["cannot access 'Hidden': it is internal"]
    );
    assert_eq!(
        fixture.diagnostics(
            "import lib.Hidden\n\
             fun use(): Hidden? = null\n"
        ),
        ["cannot access 'Hidden': it is internal"]
    );
    assert_eq!(
        fixture.diagnostics(
            "import lib.Visible\n\
             fun use(value: Visible): Int = value.value\n"
        ),
        Vec::<String>::new()
    );

    let alias_diagnostics = fixture.diagnostics(
        "import lib.Hidden as Alias\n\
         fun use(value: Alias): Int = 0\n",
    );
    assert_eq!(alias_diagnostics, ["cannot access 'Alias': it is internal"]);
    assert_no_underlying_name(&alias_diagnostics, "Hidden");

    let nested_alias_diagnostics = fixture.diagnostics(
        "import lib.Parent.ProtectedBox as Guard\n\
         fun use(value: Guard): Int = 0\n",
    );
    assert_eq!(
        nested_alias_diagnostics,
        ["cannot access 'Guard': it is protected"]
    );
    assert_no_underlying_name(&nested_alias_diagnostics, "ProtectedBox");

    assert_eq!(
        fixture.diagnostics(
            "import lib.Parent\n\
             class Child : Parent() {\n\
                 private fun use(value: ProtectedBox): ProtectedBox = value\n\
             }\n"
        ),
        Vec::<String>::new()
    );

    assert_eq!(
        fixture.diagnostics(
            "import lib.Hidden\n\
             class Owner {\n\
                 class Hidden\n\
                 fun use(value: Hidden): Hidden = value\n\
             }\n"
        ),
        Vec::<String>::new()
    );

    assert_eq!(
        fixture.diagnostics(
            "import lib.Hidden\n\
             fun use(): Int {\n\
                 val value: Hidden? = null\n\
                 return 0\n\
             }\n"
        ),
        ["cannot access 'Hidden': it is internal"]
    );
}
