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

pub const NATIVE_ABI_SYMBOLS: Case = Case {
    stem: "multimoduleNativeAbiSymbols",
    source: "// TARGET_BACKEND: NATIVE\n\
             // MODULE: left\npackage shared\ninternal class Token(val value: String)\ninternal value class Packed(val value: String)\n\
             // MODULE: right\npackage shared\ninternal class Token(val value: String)\ninternal value class Packed(val value: String)\n\
             // MODULE: main(left, right)\nfun box() = \"OK\"\n",
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
    NATIVE_ABI_SYMBOLS,
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
