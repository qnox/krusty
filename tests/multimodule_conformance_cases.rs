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

pub const LOCAL_INCREMENT_SHAPES: Case = Case {
    stem: "multimoduleLocalIncrementShapes",
    source: "// MODULE: lib\n\
             fun exerciseUpdates(): String {\n\
             \x20   var local = 1\n\
             \x20   val oldLocal = local++\n\
             \x20   ++local\n\
             \x20   if (oldLocal != 1 || local != 3) return \"local\"\n\
             \x20   return \"OK\"\n\
             }\n\
             // MODULE: main(lib)\n\
             fun box() = exerciseUpdates()\n",
};

/// The consumer's `accept` signature requires both the outer `Any` and inner `String` arguments
/// reconstructed from dependency metadata; `innerValue` also exercises the latter through the
/// dependency's emitted member body. Both arguments are references because primitive boxing is a
/// separate backend representation concern, not part of this metadata regression.
pub const INNER_CLASS_OUTER_TYPE: Case = Case {
    stem: "multimoduleInnerClassOuterType",
    source: "// MODULE: lib\n\
             class Outer<T>(private val outer: T) {\n\
             \x20   inner class Inner<U>(private val inner: U) {\n\
             \x20       fun innerValue(): U = inner\n\
             \x20   }\n\
             }\n\
             fun value(): Outer<Any>.Inner<String> {\n\
             \x20   val outer = Outer<Any>(\"O\")\n\
             \x20   return outer.Inner(\"K\")\n\
             }\n\
             // MODULE: main(lib)\n\
             fun accept(value: Outer<Any>.Inner<String>): String = value.innerValue()\n\
             fun box(): String {\n\
             \x20   val inner: String = accept(value())\n\
             \x20   return if (inner == \"K\") \"OK\" else \"fail $inner\"\n\
             }\n",
};

/// A member extension has two independent receivers across the KLIB boundary: the imported class
/// instance dispatches the member from the consumer's extension body, while the source extension
/// receiver remains in the function parameters. The library constructs its own class so this case
/// isolates receiver ABI from cross-KLIB constructor support.
pub const MEMBER_EXTENSION_RECEIVERS: Case = Case {
    stem: "multimoduleMemberExtensionReceivers",
    source: "// MODULE: lib\n\
             class Host(private val prefix: String) {\n\
             \x20   fun String.decorate(suffix: String): String = prefix + this + suffix\n\
             }\n\
             fun host(): Host = Host(\"O\")\n\
             // MODULE: main(lib)\n\
             fun Host.decorateOk(): String = \"K\".decorate(\"!\")\n\
             fun box(): String {\n\
             \x20   val decorated = host().decorateOk()\n\
             \x20   return if (decorated == \"OK!\") \"OK\" else decorated\n\
             }\n",
};

/// Reified substitutions cross a real separately compiled KLIB for every callable origin. All
/// arguments are inferred at the consumer call site, so success requires the selected call—not
/// source spelling or an origin-specific lowering path—to publish the runtime type identity.
pub const REIFIED_CALLABLE_ORIGINS: Case = Case {
    stem: "multimoduleReifiedCallableOrigins",
    source: "// MODULE: lib\n\
             inline fun <reified T> top(value: T, probe: Any): Boolean = probe is T\n\
             inline fun <reified T> T.topExtension(probe: Any): Boolean = probe is T\n\
             class Host {\n\
             \x20   inline fun <reified T> member(value: T, probe: Any): Boolean = probe is T\n\
             \x20   inline fun <reified T> T.memberExtension(probe: Any): Boolean = probe is T\n\
             }\n\
             // MODULE: main(lib)\n\
             fun box(): String {\n\
             \x20   val host = Host()\n\
             \x20   if (!top(\"top\", \"probe\")) return \"top\"\n\
             \x20   if (!\"extension\".topExtension(\"probe\")) return \"extension\"\n\
             \x20   if (!host.member(\"member\", \"probe\")) return \"member\"\n\
             \x20   if (!host.run { \"member extension\".memberExtension(\"probe\") }) return \"member extension\"\n\
             \x20   return \"OK\"\n\
             }\n",
};

pub const DEPENDENCY_FUNCTION_IMPLEMENTATION_MARKERS: Case = Case {
    stem: "multimoduleDependencyFunctionImplementationMarkers",
    source: "// TARGET_BACKEND: NATIVE\n\
             // MODULE: lib\n\
             interface ExternalFn : (Int) -> Int\n\
             // MODULE: main(lib)\n\
             interface LocalFn : ExternalFn\n\
             class Impl : ExternalFn { override fun invoke(value: Int) = value }\n\
             class LocalImpl : LocalFn { override fun invoke(value: Int) = value }\n\
             fun box(): String {\n\
             \x20   val direct: Any = Impl()\n\
             \x20   val throughLocal: Any = LocalImpl()\n\
             \x20   if (direct !is Function<*>) return \"direct common\"\n\
             \x20   if (direct !is Function1<*, *>) return \"direct arity\"\n\
             \x20   if (throughLocal !is Function<*>) return \"local common\"\n\
             \x20   if (throughLocal !is Function1<*, *>) return \"local arity\"\n\
             \x20   return \"OK\"\n\
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
    LOCAL_INCREMENT_SHAPES,
    INNER_CLASS_OUTER_TYPE,
    MEMBER_EXTENSION_RECEIVERS,
    REIFIED_CALLABLE_ORIGINS,
    DEPENDENCY_FUNCTION_IMPLEMENTATION_MARKERS,
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
