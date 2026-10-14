//! Backend-neutral multimodule box fixtures shared by every runnable target.
//!
//! Backend modules own provisioning, compilation, and execution. The Kotlin source graph lives
//! here so adding a portable conformance claim cannot silently cover only one backend.

#[derive(Clone, Copy)]
pub struct Case {
    pub stem: &'static str,
    pub source: &'static str,
}

pub const TOP_LEVEL_DEPENDENCY: Case = Case {
    stem: "multimoduleTopLevelDependency",
    source: "// MODULE: lib\nfun answer() = \"OK\"\n\
             // MODULE: main(lib)\nfun box() = answer()\n",
};

pub const DEPENDS_ON: Case = Case {
    stem: "multimoduleDependsOn",
    source: "// MODULE: common\nexpect fun answer(): String\n\
             // MODULE: main()()(common)\nactual fun answer() = \"OK\"\nfun box() = answer()\n",
};

pub const EXPECT_DEFAULT_PROVIDER: Case = Case {
    stem: "multimoduleExpectDefaultProvider",
    source: "// MODULE: common\nexpect fun answer(value: String = \"OK\"): String\n\
             // MODULE: main()()(common)\n\
             // FILE: call.kt\nfun box() = answer()\n\
             // FILE: actual.kt\nactual fun answer(value: String) = value\n",
};

pub const DEPENDS_ON_FRIEND: Case = Case {
    stem: "multimoduleDependsOnFriendDependency",
    source: "// MODULE: friend\npackage shared\ninternal fun answer() = \"OK\"\n\
             // MODULE: common(friend)(friend)\npackage common\nimport shared.answer\nfun commonAnswer() = answer()\n\
             // MODULE: main()()(common)\nimport common.commonAnswer\nfun box() = commonAnswer()\n",
};

pub const FRIEND_DEPENDENCY: Case = Case {
    stem: "multimoduleFriendDependency",
    source:
        "// MODULE: friend\npackage shared\ninternal fun answer() = \"OK\"\n\
             // MODULE: sibling\npackage shared\ninternal fun unrelated() = \"not a friend\"\n\
             // MODULE: main(friend, sibling)(friend)\nimport shared.answer\nfun box() = answer()\n",
};

pub const EQUAL_INTERNAL_SIGNATURES: Case = Case {
    stem: "multimoduleEqualInternalSignatures",
    source:
        "// MODULE: friend\npackage shared\ninternal fun answer() = \"OK\"\n\
             // MODULE: sibling\npackage shared\ninternal fun answer() = \"wrong module\"\n\
             // MODULE: main(friend, sibling)(friend)\nimport shared.answer\nfun box() = answer()\n",
};

pub const DEFAULT_CALLABLE_IDS: Case = Case {
    stem: "multimoduleDefaultCallableIds",
    source: "// MODULE: lib\n\
             // FILE: libRead.kt\nfun read() = answer()\n\
             // FILE: libDefault.kt\nfun answer(prefix: String = \"O\", value: String = prefix + \"K\") = value\n\
             // MODULE: main(lib)\n\
             // FILE: mainBox.kt\nfun box(): String { local(); return read() }\n\
             // FILE: mainDefault.kt\nfun local(prefix: String = \"lo\", value: String = prefix + \"cal\") = value\n",
};

pub const PROPERTY_IDS: Case = Case {
    stem: "multimodulePropertyIds",
    source: "// MODULE: lib\n\
             // FILE: libValue.kt\nval answer = \"OK\"\n\
             // FILE: libRead.kt\nfun read() = answer\n\
             // MODULE: main(lib)\n\
             // FILE: mainValue.kt\nval local = \"local\"\n\
             // FILE: mainBox.kt\nfun box(): String { if (local != \"local\") return \"fail\"; return read() }\n",
};

pub const FORWARD_PROPERTY_IDS: Case = Case {
    stem: "multimoduleForwardPropertyIds",
    source: "// MODULE: lib\n\
             // FILE: libRead.kt\nfun read() = answer\n\
             // FILE: libValue.kt\nval answer = \"OK\"\n\
             // MODULE: main(lib)\nfun box() = read()\n",
};

pub const LOCAL_FUNCTION_SYMBOLS: Case = Case {
    stem: "multimoduleLocalFunctionSymbols",
    source: "// MODULE: lib\n\
             fun fromLib(): String {\n\
                 fun local() = \"O\"\n\
                 return local()\n\
             }\n\
             // MODULE: main(lib)\n\
             fun box(): String {\n\
                 fun local() = \"K\"\n\
                 return fromLib() + local()\n\
             }\n",
};

pub const NATIVE_ABI_SYMBOLS: Case = Case {
    stem: "multimoduleNativeAbiSymbols",
    source: "// TARGET_BACKEND: NATIVE\n\
             // MODULE: left\npackage shared\ninternal class Token(val value: String)\ninternal value class Packed(val value: String)\n\
             // MODULE: right\npackage shared\ninternal class Token(val value: String)\ninternal value class Packed(val value: String)\n\
             // MODULE: main(left, right)\nfun box() = \"OK\"\n",
};

pub const FLOATING_BITS: Case = Case {
    stem: "multimoduleFloatingBits",
    source: "// MODULE: bits\n\
             fun floatBits(value: Float) = value.toRawBits()\n\
             fun floatCanonicalBits(value: Float) = value.toBits()\n\
             fun floatFromBits(value: Int) = Float.fromBits(value)\n\
             fun doubleBits(value: Double) = value.toRawBits()\n\
             fun doubleCanonicalBits(value: Double) = value.toBits()\n\
             fun doubleFromBits(value: Long) = Double.fromBits(value)\n\
             // MODULE: main(bits)\n\
             fun box(): String {\n\
             \x20   if (floatBits(1.0f) != 1065353216) return \"float\"\n\
             \x20   if (doubleFromBits(4607182418800017408L) != 1.0) return \"double\"\n\
             \x20   val floatNan = floatFromBits(2139095041)\n\
             \x20   if (floatBits(floatNan) != 2139095041) return \"float raw nan\"\n\
             \x20   if (floatCanonicalBits(floatNan) != 2143289344) return \"float canonical nan\"\n\
             \x20   val doubleNan = doubleFromBits(9218868437227405313L)\n\
             \x20   if (doubleBits(doubleNan) != 9218868437227405313L) return \"double raw nan\"\n\
             \x20   if (doubleCanonicalBits(doubleNan) != 9221120237041090560L) return \"double canonical nan\"\n\
             \x20   return \"OK\"\n\
             }\n",
};

pub const FLOATING_BIT_REFERENCES: Case = Case {
    stem: "multimoduleFloatingBitReferences",
    source: "// MODULE: bits\n\
             @Suppress(\"INVISIBLE_REFERENCE\", \"INVISIBLE_MEMBER\")\n\
             @kotlin.internal.InlineOnly\n\
             inline fun <T, R> T.throughReference(block: (T) -> R): R = block(this)\n\
             fun floatFromBitsReference(value: Int): Float = value.throughReference(Float::fromBits)\n\
             fun doubleFromBitsReference(value: Long): Double = value.throughReference(Double::fromBits)\n\
             // MODULE: main(bits)\n\
             fun box(): String {\n\
             \x20   if (floatFromBitsReference(1065353216) != 1.0f) return \"float reference\"\n\
             \x20   if (doubleFromBitsReference(4607182418800017408L) != 1.0) return \"double reference\"\n\
             \x20   return \"OK\"\n\
             }\n",
};

pub const OVERLAPPING_BITS: Case = Case {
    stem: "multimoduleOverlappingBits",
    source: "// MODULE: bits\n\
             fun raw(value: Float): Int = value.toRawBits()\n\
             // MODULE: main(bits)\n\
             fun canonical(value: Float): Int = value.toBits()\n\
             fun box(): String {\n\
             \x20   if (raw(1.0f) != 1065353216) return \"raw\"\n\
             \x20   if (canonical(1.0f) != 1065353216) return \"canonical\"\n\
             \x20   return \"OK\"\n\
             }\n",
};

/// Every portable multimodule claim. Each backend adapter iterates this table in full.
pub const ALL: &[Case] = &[
    TOP_LEVEL_DEPENDENCY,
    DEPENDS_ON,
    EXPECT_DEFAULT_PROVIDER,
    DEPENDS_ON_FRIEND,
    FRIEND_DEPENDENCY,
    EQUAL_INTERNAL_SIGNATURES,
    DEFAULT_CALLABLE_IDS,
    PROPERTY_IDS,
    FORWARD_PROPERTY_IDS,
    LOCAL_FUNCTION_SYMBOLS,
    NATIVE_ABI_SYMBOLS,
    FLOATING_BITS,
    FLOATING_BIT_REFERENCES,
    OVERLAPPING_BITS,
];

#[test]
fn multimodule_cases_run_through_the_matrix_selected_backend() {
    let runner = std::env::var("KRUSTY_MULTIMODULE_CONFORMANCE_RUNNER")
        .expect("multimodule conformance requires an explicit matrix-selected runner");
    for case in ALL {
        match runner.as_str() {
            "jvm" => super::kotlin_box_ir_jvm_conformance::run_multimodule_case(case),
            "native" => super::kotlin_box_native_conformance::run_multimodule_case(case),
            "wasm" => super::kotlin_box_wasm_conformance::run_multimodule_case(case),
            other => panic!("unknown multimodule conformance runner {other}"),
        }
    }
}
