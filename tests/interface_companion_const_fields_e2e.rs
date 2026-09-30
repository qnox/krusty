//! An interface companion's `const val` storage, measured against kotlinc 2.4.20.
//!
//! A public or internal const is `public static final` + `ConstantValue` on the interface (after
//! `Companion`) and again on the companion. A private const cannot be an interface field, so it
//! stays `private static final` on the companion only. Companion fields follow source order after
//! `$$INSTANCE`, interleaving consts with instance backing fields. A class companion still moves
//! every const onto the outer class alone.
use super::common;

fn krusty_bytes(src: &str, stem: &str, class_internal: &str) -> Vec<u8> {
    let cp = [common::stdlib_jar()];
    let classes = common::compile_in_process_metadata_cp(src, stem, &cp).unwrap_or_else(|| {
        let diagnostics = common::front_end_diagnostics(src, &cp, None);
        panic!("{class_internal}: krusty declined the source; diagnostics: {diagnostics:?}")
    });
    classes
        .into_iter()
        .find(|(name, _)| name == class_internal)
        .map(|(_, bytes)| bytes)
        .unwrap_or_else(|| panic!("{class_internal} was not emitted"))
}

fn kotlinc_bytes(src: &str, stem: &str, class_internal: &str) -> Option<Vec<u8>> {
    common::java_home();
    let dir = common::scratch_dir()?;
    let out = dir.join("out");
    std::fs::create_dir_all(&out).ok()?;
    let kt = dir.join(format!("{stem}.kt"));
    std::fs::write(&kt, src).ok()?;
    let args = vec![
        kt.to_string_lossy().into_owned(),
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args)?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let bytes = std::fs::read(out.join(format!("{class_internal}.class"))).ok();
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

fn assert_byte_identical(src: &str, class_internal: &str) {
    let stem = class_internal
        .rsplit('/')
        .next()
        .unwrap()
        .split('$')
        .next()
        .unwrap();
    let Some(reference) = kotlinc_bytes(src, stem, class_internal) else {
        eprintln!("skip ({class_internal}: provisioned kotlinc unavailable)");
        return;
    };
    let compiled = krusty_bytes(src, stem, class_internal);
    assert_eq!(
        compiled,
        reference,
        "{class_internal} must be byte-for-byte identical to kotlinc (krusty {} B, kotlinc {} B)",
        compiled.len(),
        reference.len(),
    );
}

const VISIBILITY: &str = "\
package demo
interface I {
    companion object {
        private const val HIDDEN = \"h\"
        internal const val INSIDE = \"i\"
        const val PUBLIC = \"p\"
    }
}
";

const CONST_THEN_VAL: &str = "\
package demo
interface M {
    companion object {
        const val C = \"c\"
        val runtime: String = \"r\"
        private const val H = \"h\"
    }
}
";

const VAL_THEN_CONST: &str = "\
package demo
interface R {
    companion object {
        val runtime: String = \"r\"
        const val C = \"c\"
        private const val H = \"h\"
    }
}
";

const INT_CONST: &str = "\
package demo
interface N {
    companion object {
        const val N = 1
        private const val P = 2
    }
}
";

#[test]
fn interface_companion_const_visibility_owner_is_byte_identical() {
    assert_byte_identical(VISIBILITY, "demo/I");
}

#[test]
fn interface_companion_const_visibility_companion_is_byte_identical() {
    assert_byte_identical(VISIBILITY, "demo/I$Companion");
}

#[test]
fn interface_companion_const_before_val_owner_is_byte_identical() {
    assert_byte_identical(CONST_THEN_VAL, "demo/M");
}

#[test]
fn interface_companion_const_before_val_companion_is_byte_identical() {
    assert_byte_identical(CONST_THEN_VAL, "demo/M$Companion");
}

#[test]
fn interface_companion_val_before_const_owner_is_byte_identical() {
    assert_byte_identical(VAL_THEN_CONST, "demo/R");
}

#[test]
fn interface_companion_val_before_const_companion_is_byte_identical() {
    assert_byte_identical(VAL_THEN_CONST, "demo/R$Companion");
}

#[test]
fn interface_companion_int_const_owner_is_byte_identical() {
    assert_byte_identical(INT_CONST, "demo/N");
}

#[test]
fn interface_companion_int_const_companion_is_byte_identical() {
    assert_byte_identical(INT_CONST, "demo/N$Companion");
}
