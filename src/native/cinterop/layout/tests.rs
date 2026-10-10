//! Each expected layout below was worked out by hand from the psABI rules in the module comment
//! and agrees with clang's `-fdump-record-layouts` for the same source on each target.

use super::super::compiler_headers::preprocessor_for;
use super::super::declarations::{parse, Header};
use super::super::model::Type;
use super::*;
use crate::native::target::Os;

const X86_64: NativeTarget = NativeTarget::new(Arch::X86_64, Os::Linux);
const AARCH64: NativeTarget = NativeTarget::new(Arch::Aarch64, Os::Linux);
const RISCV64: NativeTarget = NativeTarget::new(Arch::Riscv64, Os::Linux);
const ALL: [NativeTarget; 3] = [X86_64, AARCH64, RISCV64];

fn header(target: NativeTarget, source: &str) -> Header {
    let mut preprocessor = preprocessor_for(target, Vec::new(), Vec::new()).expect("predefines");
    let tokens = preprocessor.run("t.h", source).expect("preprocesses");
    match parse(&tokens, target) {
        Ok(header) => header,
        Err(error) => panic!("{error}"),
    }
}

/// `size/align [offset, …]` for the record tagged `tag`, offsets in bits.
fn record(target: NativeTarget, source: &str, tag: &str) -> String {
    let mut header = header(target, source);
    let id = header
        .declarations
        .records
        .iter()
        .position(|record| record.tag.as_deref() == Some(tag))
        .expect("the record is declared");
    let layout = header
        .layouts
        .record(&header.declarations, RecordId(id as u32))
        .expect("lays out");
    format!(
        "{}/{} {:?}",
        layout.size, layout.align, layout.field_offsets
    )
}

/// `size/align` of the typedef `name`.
fn typedef(target: NativeTarget, source: &str, name: &str) -> String {
    let mut header = header(target, source);
    let ty = header
        .declarations
        .typedefs
        .iter()
        .position(|typedef| &*typedef.name == name)
        .map(|index| {
            header
                .declarations
                .intern(Type::Typedef(super::super::model::TypedefId(index as u32)))
        })
        .expect("the typedef is declared");
    let layout = header
        .layouts
        .layout(&header.declarations, ty)
        .expect("lays out");
    format!("{}/{}", layout.size, layout.align)
}

#[test]
fn fields_align_to_their_types_and_the_record_to_its_largest() {
    let source = "struct basic { char c; int i; short s; long double ld; char tail; };";
    for target in ALL {
        assert_eq!(
            record(target, source, "basic"),
            "48/16 [0, 32, 64, 128, 256]",
            "{target}"
        );
    }
}

#[test]
fn a_bitfield_moves_only_when_it_would_cross_its_types_alignment() {
    let source = "struct bits { unsigned a : 3; unsigned b : 30; char c;\n\
                  unsigned long long d : 40; unsigned long long e : 30; short f : 9; };";
    for target in ALL {
        assert_eq!(
            record(target, source, "bits"),
            "24/8 [0, 32, 64, 72, 128, 160]",
            "{target}"
        );
    }
}

#[test]
fn unnamed_bitfields_raise_the_alignment_only_on_aarch64() {
    let source = "struct zero { char a; int : 0; char b; };\n\
                  struct unnamed { char a; int : 4; };\n\
                  struct named_after_zero { char a; long : 0; char b : 2; };";
    for target in [X86_64, RISCV64] {
        assert_eq!(record(target, source, "zero"), "5/1 [0, 32, 32]");
        assert_eq!(record(target, source, "unnamed"), "2/1 [0, 8]");
        assert_eq!(
            record(target, source, "named_after_zero"),
            "9/1 [0, 64, 64]"
        );
    }
    assert_eq!(record(AARCH64, source, "zero"), "8/4 [0, 32, 32]");
    assert_eq!(record(AARCH64, source, "unnamed"), "4/4 [0, 8]");
    assert_eq!(
        record(AARCH64, source, "named_after_zero"),
        "16/8 [0, 64, 64]"
    );
}

#[test]
fn packed_and_aligned_attributes_change_field_and_record_alignment() {
    let source =
        "struct __attribute__((packed)) packed { char a; int b; short c : 5; int d : 30;\n\
                      long e; };\n\
                  struct aligned { char a; int b __attribute__((aligned(16))); char c; }\n\
                      __attribute__((aligned(32)));\n\
                  struct mixed { char a; int b __attribute__((aligned(2))); }\n\
                      __attribute__((packed));\n\
                  struct aligned_bits { char a; int b : 4 __attribute__((aligned(4))); };\n\
                  struct epoll_like { unsigned int events; unsigned long data; }\n\
                      __attribute__ ((__packed__));";
    for target in ALL {
        assert_eq!(record(target, source, "packed"), "18/1 [0, 8, 40, 45, 80]");
        assert_eq!(record(target, source, "aligned"), "32/32 [0, 128, 160]");
        assert_eq!(record(target, source, "mixed"), "6/2 [0, 16]");
        assert_eq!(record(target, source, "aligned_bits"), "8/4 [0, 32]");
        assert_eq!(record(target, source, "epoll_like"), "12/1 [0, 32]");
    }
}

#[test]
fn unions_anonymous_members_and_flexible_arrays() {
    let source = "union u { char a; int b : 20; long double c; short d[3]; };\n\
                  struct flex { int n; struct { char x; double y; };\n\
                      union { short s; char z[3]; }; char data[]; };";
    for target in ALL {
        assert_eq!(record(target, source, "u"), "16/16 [0, 0, 0, 0]");
        assert_eq!(record(target, source, "flex"), "32/8 [0, 64, 192, 224]");
    }
}

#[test]
fn a_typedefs_alignment_attribute_replaces_its_types() {
    let source = "typedef long long4 __attribute__((aligned(4)));\n\
                  struct lowered { char c; long4 l; };";
    for target in ALL {
        assert_eq!(record(target, source, "lowered"), "12/4 [0, 32]");
        assert_eq!(typedef(target, source, "long4"), "8/4");
    }
}

#[test]
fn builtin_atomic_complex_enum_and_vector_types_per_target() {
    let source = "typedef __builtin_va_list va;\n\
                  struct three { char a[3]; };\n\
                  typedef _Atomic(struct three) atomic3;\n\
                  typedef _Complex long double cld;\n\
                  typedef enum small { S = 1 } __attribute__((packed)) small;\n\
                  typedef enum negative { NEG = -1, POS = 0x7fffffff } negative;\n\
                  typedef enum wide { W = 0x100000000 } wide;\n\
                  typedef int v3 __attribute__((vector_size(12)));\n\
                  typedef int v8 __attribute__((vector_size(32)));";
    let summary = |target| {
        [
            "va", "atomic3", "cld", "small", "negative", "wide", "v3", "v8",
        ]
        .map(|name| format!("{name}={}", typedef(target, source, name)))
        .join(" ")
    };
    assert_eq!(
        summary(X86_64),
        "va=24/8 atomic3=4/4 cld=32/16 small=1/1 negative=4/4 wide=8/8 v3=16/16 v8=32/32"
    );
    assert_eq!(
        summary(AARCH64),
        "va=32/8 atomic3=4/4 cld=32/16 small=1/1 negative=4/4 wide=8/8 v3=16/16 v8=32/16"
    );
    assert_eq!(
        summary(RISCV64),
        "va=8/8 atomic3=4/4 cld=32/16 small=1/1 negative=4/4 wide=8/8 v3=16/16 v8=32/32"
    );
}

#[test]
fn an_incomplete_record_or_too_wide_bitfield_has_no_layout() {
    let mut incomplete = header(X86_64, "struct opaque;");
    assert_eq!(
        incomplete
            .layouts
            .record(&incomplete.declarations, RecordId(0))
            .expect_err("incomplete"),
        LayoutError("`struct opaque` is an incomplete type".to_string())
    );
    let mut wide = header(X86_64, "struct w { char c : 9; };");
    assert_eq!(
        wide.layouts
            .record(&wide.declarations, RecordId(0))
            .expect_err("too wide"),
        LayoutError("bitfield `c` is wider than its type `char`".to_string())
    );
}
