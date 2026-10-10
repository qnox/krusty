//! What a C compiler supplies before the C library's headers: the target's predefined macros and
//! the freestanding headers (`stddef.h`, `stdarg.h`, `limits.h`, …). krusty carries both inside
//! the compiler, per target, so reading a library's headers needs no C compiler installed.
//!
//! The predefined macros are clang's for the target at `-std=gnu11`, which is what Kotlin/Native's
//! cinterop parses headers with, so a header's conditional code takes the same branches here.

use super::preprocessor::{IncludeDir, PreprocessError, Preprocessor};
use crate::native::target::{Arch, NativeTarget, Os};

/// The freestanding headers, written against the predefined macros rather than any one target.
const HEADERS: &[(&str, &str)] = &[
    ("float.h", include_str!("include/float.h")),
    ("iso646.h", include_str!("include/iso646.h")),
    ("limits.h", include_str!("include/limits.h")),
    ("stdalign.h", include_str!("include/stdalign.h")),
    ("stdarg.h", include_str!("include/stdarg.h")),
    ("stdatomic.h", include_str!("include/stdatomic.h")),
    ("stdbool.h", include_str!("include/stdbool.h")),
    ("stddef.h", include_str!("include/stddef.h")),
    ("stdint.h", include_str!("include/stdint.h")),
    ("stdnoreturn.h", include_str!("include/stdnoreturn.h")),
];

fn predefined_macros(target: NativeTarget) -> &'static str {
    match (target.os, target.arch) {
        (Os::Linux, Arch::X86_64) => include_str!("predefined/x86_64-unknown-linux-gnu.h"),
        (Os::Linux, Arch::Aarch64) => include_str!("predefined/aarch64-unknown-linux-gnu.h"),
        (Os::Linux, Arch::Riscv64) => include_str!("predefined/riscv64-unknown-linux-gnu.h"),
    }
}

/// A preprocessor for `target`: its predefined macros, then the compiler's headers ahead of the
/// C library's directories (`system_dirs`), as a C compiler searches them.
pub(super) fn preprocessor_for(
    target: NativeTarget,
    user_dirs: Vec<IncludeDir>,
    system_dirs: Vec<IncludeDir>,
) -> Result<Preprocessor, PreprocessError> {
    let mut dirs = user_dirs;
    dirs.push(IncludeDir::Builtin(HEADERS));
    dirs.extend(system_dirs);
    let mut preprocessor = Preprocessor::new(dirs);
    preprocessor.predefine(predefined_macros(target))?;
    Ok(preprocessor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand(target: NativeTarget, source: &str) -> String {
        let mut preprocessor =
            preprocessor_for(target, Vec::new(), Vec::new()).expect("predefines");
        let tokens = preprocessor.run("t.h", source).expect("preprocesses");
        tokens
            .iter()
            .map(|token| &*token.text)
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn every_target_predefines_its_own_data_model() {
        let source = "__SIZEOF_LONG__ __SIZEOF_POINTER__ __CHAR_UNSIGNED__ __x86_64__ __aarch64__";
        assert_eq!(
            expand(NativeTarget::new(Arch::X86_64, Os::Linux), source),
            "8 8 __CHAR_UNSIGNED__ 1 __aarch64__"
        );
        assert_eq!(
            expand(NativeTarget::new(Arch::Aarch64, Os::Linux), source),
            "8 8 1 __x86_64__ 1"
        );
    }

    #[test]
    fn the_freestanding_headers_define_their_names_from_the_target() {
        let source = "#include <stddef.h>\n#include <limits.h>\n#include <stdbool.h>\n\
                      size_t; INT_MAX CHAR_MIN bool";
        assert_eq!(
            expand(NativeTarget::new(Arch::Aarch64, Os::Linux), source),
            "typedef long unsigned int size_t ; typedef long int ptrdiff_t ; typedef unsigned int wchar_t ; \
             typedef struct { long long __clang_max_align_nonce1 __attribute__ ( ( __aligned__ ( \
             __alignof__ ( long long ) ) ) ) ; long double __clang_max_align_nonce2 __attribute__ \
             ( ( __aligned__ ( __alignof__ ( long double ) ) ) ) ; } max_align_t ; size_t ; \
             2147483647 0 _Bool"
        );
    }
}
