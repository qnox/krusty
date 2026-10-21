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

/// A local class inherits and overrides a member-extension property whose expect receiver must
/// remain a semantic identity when the platform module provides it through an actual typealias.
/// Every backend consumes this exact source through the common matrix inventory.
pub const LOCAL_MEMBER_EXTENSION_EXPECT_RECEIVER: Case = Case {
    stem: "multimoduleLocalMemberExtensionExpectReceiver",
    source: "// MODULE: common\n\
             expect class Receiver {\n\
             \x20   val prefix: String\n\
             }\n\
             interface Left {\n\
             \x20   fun read(receiver: Receiver): String\n\
             \x20   val Receiver.suffix: String\n\
             }\n\
             interface Right {\n\
             \x20   fun read(receiver: Receiver): String\n\
             \x20   val Receiver.suffix: String\n\
             }\n\
             interface Both : Left, Right\n\
             fun exercise(receiver: Receiver): String {\n\
             \x20   class Local : Both {\n\
             \x20       override fun read(receiver: Receiver): String = receiver.prefix\n\
             \x20       override val Receiver.suffix: String get() = \"K\"\n\
             \x20   }\n\
             \x20   val local = Local()\n\
             \x20   return with(local) { local.read(receiver) + receiver.suffix }\n\
             }\n\
             // MODULE: main()()(common)\n\
             class ActualReceiver(val prefix: String)\n\
             actual typealias Receiver = ActualReceiver\n\
             fun box(): String = exercise(ActualReceiver(\"O\"))\n",
};

/// Reified substitutions cross a real separately compiled KLIB for every callable origin. All
/// arguments are inferred at the consumer call site, so success requires the selected call—not
/// source spelling or an origin-specific lowering path—to publish the runtime type identity. The
/// library constructs its own class so cross-KLIB constructor emission remains a separate concern.
pub const REIFIED_CALLABLE_ORIGINS: Case = Case {
    stem: "multimoduleReifiedCallableOrigins",
    source: "// LANGUAGE: +ContextParameters\n\
             // MODULE: lib\n\
             inline fun <reified T> top(value: T, probe: Any): Boolean = probe is T\n\
             inline fun <reified T> Any?.topExtension(): Boolean = this is T\n\
             context(marker: String)\n\
             inline fun <reified T> contextual(probe: Any): Boolean = probe is T\n\
             inline fun <reified T> defaulted(probe: Any? = null): Boolean = probe is T\n\
             class Host {\n\
             \x20   inline fun <reified T> member(value: T, probe: Any): Boolean = probe is T\n\
             \x20   inline fun <reified T> Any?.memberExtension(): Boolean = this is T\n\
             }\n\
             fun host(): Host = Host()\n\
             // MODULE: main(lib)\n\
             fun box(): String {\n\
             \x20   val host = host()\n\
             \x20   if (!top(\"top\", \"probe\")) return \"top\"\n\
             \x20   if (!top(probe = \"named probe\", value = \"named\")) return \"named\"\n\
             \x20   if (!\"extension\".topExtension<String>()) return \"extension\"\n\
             \x20   with(\"context\") {\n\
             \x20       if (!contextual<String>(\"probe\")) return \"context\"\n\
             \x20   }\n\
             \x20   if (defaulted<String>()) return \"default\"\n\
             \x20   if (!host.member(\"member\", \"probe\")) return \"member\"\n\
             \x20   if (!host.run { \"member extension\".memberExtension<String>() }) return \"member extension\"\n\
             \x20   return \"OK\"\n\
             }\n",
};

pub const DEPENDENCY_FUNCTION_IMPLEMENTATION_MARKERS: Case = Case {
    stem: "multimoduleDependencyFunctionImplementationMarkers",
    source: "// MODULE: lib\n\
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
/// `also` and `apply` from the target stdlib expand from their prepared inline bodies: each answers
/// its receiver after the lambda ran, a lambda mutates a caller `var`, and a `return` inside
/// `also`'s lambda leaves the enclosing function. The library module calls them too, so the
/// expansion reaches a dependency's own body as well as the consumer's.
pub const RECEIVER_RETURNING_SCOPE_FUNCTIONS: Case = Case {
    stem: "multimoduleReceiverReturningScopeFunctions",
    source: "// MODULE: lib\n\
             fun tagged(value: String): String = value.also { }\n\
             fun appliedLength(value: String): Int {\n\
             \x20   var total = 0\n\
             \x20   value.apply { total += length }\n\
             \x20   return total\n\
             }\n\
             // MODULE: main(lib)\n\
             fun early(value: String): String {\n\
             \x20   value.also { return \"OK\" }\n\
             \x20   return \"no non-local return\"\n\
             }\n\
             fun box(): String {\n\
             \x20   if (tagged(\"lib\") != \"lib\") return \"library also\"\n\
             \x20   if (appliedLength(\"abc\") != 3) return \"library apply\"\n\
             \x20   var seen = \"\"\n\
             \x20   val kept = \"K\".also { seen = it }\n\
             \x20   if (kept != \"K\" || seen != \"K\") return \"also\"\n\
             \x20   var count = 0\n\
             \x20   val applied = \"ab\".apply { count += length }\n\
             \x20   if (applied != \"ab\" || count != 2) return \"apply\"\n\
             \x20   return early(\"unused\")\n\
             }\n",
};

/// A library's reified inline function whose body the library serializes no template for (its
/// `for` loop). The declaration has no legal direct call, so a target that expands dependency
/// inline bodies in IR must reject the consumer rather than call it. Not in [`ALL`]: each such
/// target asserts its exact rejection.
pub const DECLINED_REQUIRED_INLINE_BODY: Case = Case {
    stem: "multimoduleDeclinedRequiredInlineBody",
    source: "// MODULE: lib\n\
             inline fun <reified T> countOf(value: Any, times: Int): Int {\n\
             \x20   var count = 0\n\
             \x20   for (index in 0 until times) if (value is T) count++\n\
             \x20   return count\n\
             }\n\
             // MODULE: main(lib)\n\
             fun box(): String = if (countOf<String>(\"a\", 2) == 2) \"OK\" else \"fail\"\n",
};

/// The original required-template regression is now positive: its inherited iterator reference
/// binds to the canonical declaration. An inert dependency anchors the shared multimodule oracle;
/// the main module preserves the original caller program.
pub const INLINE_STDLIB_REPEAT: Case = Case {
    stem: "inlineStdlibRepeat",
    source: "// MODULE: lib\n\
             fun dependencyMarker() = Unit\n\
             // MODULE: main(lib)\n\
             fun box(): String {\n\
             \x20   var count = 0\n\
             \x20   repeat(2) { count++ }\n\
             \x20   return if (count == 2) \"OK\" else \"fail\"\n\
             }\n",
};

/// A required inline factory whose prepared body contains a rich function reference, which
/// template lowering does not yet support. Keep this rejection contract distinct from the now
/// supported repeat caller above; neither target may emit an illegal direct call to the factory.
pub const DECLINED_STDLIB_INLINE_BODY: &str = "fun box(): String {\n\
                                               \x20   val order = compareBy<String> { it }\n\
                                               \x20   return if (order.compare(\"a\", \"b\") < 0) \"OK\" else \"fail\"\n\
                                               }\n";

/// Establish that the platform-specific rejection fixture is an ordinary valid Kotlin program.
pub fn verify_declined_stdlib_source_with_kotlinc() {
    let stdlib = super::common::stdlib_jar();
    let reference = super::box_reference_classes::reference_compile(
        DECLINED_STDLIB_INLINE_BODY,
        "declinedRequiredStdlibTemplate",
        std::slice::from_ref(&stdlib),
        krusty::conformance::BoxJdk::Full { root: None },
        "",
    )
    .expect("pinned kotlinc must compile the required-inline rejection fixture");
    let classes = reference.flatten().into_iter().collect::<Vec<_>>();
    let entry = super::common::find_box_class(&classes)
        .expect("the required-inline rejection fixture declares box()");
    assert_eq!(
        super::common::run_box(&classes, &entry, &[stdlib]),
        Some("OK".to_owned()),
        "pinned kotlinc must run the required-inline rejection fixture"
    );
}

/// The frontend rejection of a call of a required inline dependency function that has no
/// template. The callee's identity is opaque and assigned per compilation, so it is read back
/// whole from the message and must be a number; every other character is exact.
pub fn required_inline_rejection(message: &str) -> Option<u32> {
    message
        .strip_prefix(
            "internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(",
        )?
        .strip_suffix(")))")?
        .parse()
        .ok()
}

/// Portable runtime behavior reached below expanded KLIB inline templates. Each source is shared
/// with the JVM matrix row, which establishes the answer against ordinary Kotlin declarations.
pub const INLINE_TODO_DEFAULT: Case = Case {
    stem: "inlineTodoDefaultMessage",
    source: "// MODULE: lib\n\
             fun inlineBehavior(): String = try {\n\
             \x20   TODO()\n\
             } catch (e: NotImplementedError) {\n\
             \x20   if (e.message == \"An operation is not implemented.\") \"OK\" else \"fail: ${e.message}\"\n\
             }\n\
             // MODULE: main(lib)\nfun box(): String = inlineBehavior()\n",
};

pub const INLINE_ASSERT_FAILS: Case = Case {
    stem: "inlineAssertFailsWithBlock",
    source: "// WITH_STDLIB\n// MODULE: lib\n\
             import kotlin.test.*\n\
             fun inlineBehavior(): String {\n\
             \x20   val thrown = assertFailsWith<IllegalStateException> { error(\"boom\") }\n\
             \x20   if (thrown.message != \"boom\") return \"fail: ${thrown.message}\"\n\
             \x20   val any = assertFails { throw IllegalArgumentException(\"any\") }\n\
             \x20   return if (any.message == \"any\") \"OK\" else \"fail: ${any.message}\"\n\
             }\n\
             // MODULE: main(lib)\nfun box(): String = inlineBehavior()\n",
};

pub const INLINE_ASSERT_FAILS_COMPLETES: Case = Case {
    stem: "inlineAssertFailsWithCompletedBlock",
    source: "// WITH_STDLIB\n// MODULE: lib\n\
             import kotlin.test.*\n\
             fun inlineBehavior(): String = try {\n\
             \x20   assertFailsWith<IllegalStateException> { }\n\
             \x20   \"fail: no assertion error\"\n\
             } catch (e: AssertionError) {\n\
             \x20   \"OK\"\n\
             }\n\
             // MODULE: main(lib)\nfun box(): String = inlineBehavior()\n",
};

pub const INLINE_UNSIGNED_DIVISION: Case = Case {
    stem: "inlineUnsignedConstructionAndData",
    source: "// MODULE: lib\n\
             fun inlineBehavior(): String {\n\
             \x20   val big = (-1).toUInt()\n\
             \x20   if (big / 2u != 2147483647u) return \"fail: div ${big / 2u}\"\n\
             \x20   if (big % 10u != 5u) return \"fail: rem ${big % 10u}\"\n\
             \x20   if (big.toLong() != 4294967295L) return \"fail: widen ${big.toLong()}\"\n\
             \x20   return \"OK\"\n\
             }\n\
             // MODULE: main(lib)\nfun box(): String = inlineBehavior()\n",
};

pub const INLINE_NARROW_BITWISE: Case = Case {
    stem: "inlineNarrowBitwiseOperators",
    source: "// MODULE: lib\n\
             import kotlin.experimental.*\n\
             fun inlineBehavior(): String {\n\
             \x20   val a: Byte = 0x0F\n\
             \x20   val b: Byte = 0x3C\n\
             \x20   if ((a and b) != 0x0C.toByte()) return \"fail: and\"\n\
             \x20   if ((a or b) != 0x3F.toByte()) return \"fail: or\"\n\
             \x20   if (a.inv() != (-16).toByte()) return \"fail: inv\"\n\
             \x20   return \"OK\"\n\
             }\n\
             // MODULE: main(lib)\nfun box(): String = inlineBehavior()\n",
};

pub const INLINE_NARROW_RANGE_CONTAINS: Case = Case {
    stem: "inlineNarrowElementInRange",
    source: "// MODULE: lib\n\
             fun inlineBehavior(): String {\n\
             \x20   val range = 1..3\n\
             \x20   val inside: Byte = 2\n\
             \x20   val outside: Byte = 4\n\
             \x20   return if (inside in range && outside !in range) \"OK\" else \"fail\"\n\
             }\n\
             // MODULE: main(lib)\nfun box(): String = inlineBehavior()\n",
};

pub const INLINE_BUILD_STRING: Case = Case {
    stem: "inlineBuiltString",
    source: "// MODULE: lib\nfun inlineBehavior(): String = buildString { append(\"O\"); append('K') }\n\
             // MODULE: main(lib)\nfun box(): String = inlineBehavior()\n",
};

pub const INLINE_CONTEXT_UNIT_BLOCK: Case = Case {
    stem: "inlineStdlibContextUnitBlock",
    source: "// LANGUAGE: +ContextParameters\n// MODULE: lib\n\
             class Prefix(val value: String)\n\
             context(prefix: Prefix) fun read() = prefix.value\n\
             fun inlineBehavior(): String {\n\
             \x20   context(Prefix(\"O\")) {\n\
             \x20       if (read() != \"O\") return \"fail\"\n\
             \x20   }\n\
             \x20   return \"OK\"\n\
             }\n\
             // MODULE: main(lib)\nfun box(): String = inlineBehavior()\n",
};

/// Prepared stdlib copies return from conditional branches to their own inline block, not from
/// the caller. The same declaration graph checks reference/scalar results and null-failure paths.
pub const INLINE_NULL_CHECK_RETURNS: Case = Case {
    stem: "inlineNullCheckReturnTargets",
    source: r#"// MODULE: lib
fun requireText(value: String?): String = requireNotNull(value)
fun checkNumber(value: Int?): Int = checkNotNull(value)
// MODULE: main(lib)
fun box(): String {
    var calls = 0
    fun once(): String? { calls++; return "O" }
    val text = requireText(once())
    if (calls != 1 || text != "O") return "evaluation order"
    if (checkNumber(7) != 7) return "scalar result"
    try {
        requireText(null)
        return "missing require failure"
    } catch (e: Throwable) {
        if (e.message != "Required value was null.") return "require message"
    }
    try {
        checkNumber(null)
        return "missing check failure"
    } catch (e: Throwable) {
        if (e.message != "Required value was null.") return "check message"
    }
    return text + "K"
}
"#,
};

/// A String inline wrapper calls the ordinary authenticated CharSequence trim declaration.
/// Keep the caller in a separate dependency so the selected receiver fact survives publication.
pub const INLINE_TEXT_TRIM: Case = Case {
    stem: "inlineTextTrim",
    source: "// MODULE: lib\n\
             fun trimmed(value: String): String = value.trim()\n\
             // MODULE: main(lib)\n\
             fun box(): String {\n\
             \x20   if (trimmed(\"  ab  \") != \"ab\") return \"both ends\"\n\
             \x20   if (trimmed(\" \\t\\r\\n API\") != \"API\") return \"whitespace\"\n\
             \x20   if (trimmed(\"ab\") != \"ab\") return \"unchanged\"\n\
             \x20   if (trimmed(\" \\t\\n \") != \"\") return \"all whitespace\"\n\
             \x20   if (trimmed(\"\") != \"\") return \"empty\"\n\
             \x20   if (trimmed(\"\\u00A0x\\u00A0\") != \"x\") return \"Unicode whitespace\"\n\
             \x20   return \"OK\"\n\
             }\n",
};

pub const THROWABLE_TRANSPORT: Case = Case {
    stem: "multimoduleThrowableTransport",
    source: "// MODULE: lib\n\
             fun raiseHeld(value: Throwable): Nothing = throw value\n\
             fun relayHeld(value: Throwable): Nothing {\n\
             \x20   try { raiseHeld(value) } catch (caught: Throwable) { throw caught }\n\
             }\n\
             // MODULE: main(lib)\n\
             fun box(): String {\n\
             \x20   val held = Throwable(\"held\")\n\
             \x20   val caught = try { relayHeld(held) } catch (value: Throwable) { value }\n\
             \x20   if (caught !== held) return \"identity\"\n\
             \x20   if (caught.message != \"held\") return \"message\"\n\
             \x20   val empty = Throwable()\n\
             \x20   if (empty.message != null) return \"empty message\"\n\
             \x20   try { raiseHeld(empty) } catch (value: Throwable) {\n\
             \x20       if (value !== empty) return \"empty identity\"\n\
             \x20   }\n\
             \x20   return \"OK\"\n\
             }\n",
};

pub const TRY_NORMAL_COMPLETION: Case = Case {
    stem: "multimoduleTryNormalCompletion",
    source: "// MODULE: lib\n\
             fun normalValue(): Int = try { 42 } catch (value: Throwable) { -1 }\n\
             fun normalReturn(): String {\n\
             \x20   try { return \"OK\" } catch (value: Throwable) { return \"caught\" }\n\
             }\n\
             // MODULE: main(lib)\n\
             fun box(): String {\n\
             \x20   if (normalValue() != 42) return \"value\"\n\
             \x20   var count = 0\n\
             \x20   while (count < 3) {\n\
             \x20       try { count++; continue } catch (value: Throwable) { return \"loop\" }\n\
             \x20   }\n\
             \x20   return normalReturn()\n\
             }\n",
};

/// Calls the collection protocol directly, without depending on a stdlib inline template.
pub const DIRECT_COLLECTION_ITERATOR: Case = Case {
    stem: "multimoduleDirectCollectionIterator",
    source: "// MODULE: lib\n\
             // FILE: values.kt\n\
             @OptIn(ExperimentalUnsignedTypes::class)\n\
             fun unsignedValues(): Iterable<UByte> = ubyteArrayOf(1u, 255u)\n\
             // MODULE: main(lib)\n\
             // FILE: main.kt\n\
             fun box(): String {\n\
             \x20   val iterator = unsignedValues().iterator()\n\
             \x20   if (!iterator.hasNext() || iterator.next() != 1.toUByte()) return \"first\"\n\
             \x20   if (!iterator.hasNext() || iterator.next() != 255.toUByte()) return \"second\"\n\
             \x20   if (iterator.hasNext()) return \"end\"\n\
             \x20   return \"OK\"\n\
             }\n",
};

pub const ALL: &[Case] = &[
    DIRECT_COLLECTION_ITERATOR,
    INLINE_TEXT_TRIM,
    INLINE_STDLIB_REPEAT,
    THROWABLE_TRANSPORT,
    TRY_NORMAL_COMPLETION,
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
    LOCAL_MEMBER_EXTENSION_EXPECT_RECEIVER,
    REIFIED_CALLABLE_ORIGINS,
    DEPENDENCY_FUNCTION_IMPLEMENTATION_MARKERS,
    FLOATING_BITS,
    FLOATING_BIT_REFERENCES,
    OVERLAPPING_BITS,
    RECEIVER_RETURNING_SCOPE_FUNCTIONS,
    INLINE_TODO_DEFAULT,
    INLINE_ASSERT_FAILS,
    INLINE_ASSERT_FAILS_COMPLETES,
    INLINE_UNSIGNED_DIVISION,
    INLINE_NARROW_BITWISE,
    INLINE_NARROW_RANGE_CONTAINS,
    INLINE_BUILD_STRING,
    INLINE_CONTEXT_UNIT_BLOCK,
    INLINE_NULL_CHECK_RETURNS,
];

#[test]
fn multimodule_cases_run_through_the_matrix_selected_backend() {
    let runner = std::env::var("KRUSTY_MULTIMODULE_CONFORMANCE_RUNNER")
        .expect("multimodule conformance requires an explicit matrix-selected runner");
    assert!(
        matches!(runner.as_str(), "jvm" | "native" | "wasm"),
        "unknown multimodule conformance runner {runner}"
    );
    let bless = super::multimodule_ratchet::recording_requested();
    assert!(
        runner != "jvm" || !bless,
        "JVM multimodule cases have no recordable failure allowance"
    );
    for case in ALL {
        super::kotlin_box_ir_jvm_conformance::check_multimodule_reference(case);
    }
    match runner.as_str() {
        "jvm" => {
            for case in ALL {
                super::kotlin_box_ir_jvm_conformance::run_multimodule_case(case);
            }
        }
        "native" => super::multimodule_ratchet::check(
            ALL,
            vec![(
                super::box_ratchet::Platform::Native,
                ALL.iter()
                    .map(|case| {
                        (
                            case.stem.to_string(),
                            super::kotlin_box_native_conformance::run_multimodule_case(case),
                        )
                    })
                    .collect(),
            )],
            bless,
        ),
        "wasm" => super::multimodule_ratchet::check(
            ALL,
            krusty::wasm::WasmTarget::ALL
                .into_iter()
                .map(|target| {
                    let platform = match target {
                        krusty::wasm::WasmTarget::Js => super::box_ratchet::Platform::WasmJs,
                        krusty::wasm::WasmTarget::Wasi => super::box_ratchet::Platform::WasmWasi,
                    };
                    let actual = ALL
                        .iter()
                        .map(|case| {
                            (
                                case.stem.to_string(),
                                super::kotlin_box_wasm_conformance::run_multimodule_case(
                                    case, target,
                                ),
                            )
                        })
                        .collect();
                    (platform, actual)
                })
                .collect(),
            bless,
        ),
        other => panic!("unknown multimodule conformance runner {other}"),
    }
}
