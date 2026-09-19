//! A delegated property whose delegate, or whose value, crosses the scalar/reference boundary.
//!
//! Both defects produced UNVERIFIABLE bytecode — the one outcome a compiler must never emit — and
//! both were found by bucketing the box corpus's failures rather than by reading the code.
//!
//! * The delegate is stored at its own type. `val s: String by impl`, where `impl` is an `Int`,
//!   passed a raw `int` to `operator fun Any?.getValue(…)`, whose descriptor says `Object`:
//!   `Type integer … is not assignable to 'java/lang/Object'`.
//! * The accessor returns the PROPERTY's type while the operator returns the DECLARATION's.
//!   A fixture-owned generic delegate returned the `Object` that its `getValue` declaration leaves
//!   on the stack from `getAge()I`: `Type 'java/lang/Object' … is not assignable to integer`.
//!
//! Each case below is a corpus shape (`codegen/box/delegatedProperty/`) reduced to its smallest
//! form, compiled and RUN.

use super::common;

fn run(source: &str, stem: &str) -> String {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::compile_and_run_box(source, stem, &[stdlib], Some(jdk.as_path()))
        .unwrap_or_else(|| panic!("{stem} must compile and run"))
}

/// A scalar delegate reaching an operator that declares a reference receiver — top level, in a
/// class, and through an object. The operator's receiver is what decides it: one declared ON the
/// scalar keeps the value unboxed, so the boxing is not unconditional.
#[test]
fn a_scalar_delegate_is_boxed_for_a_reference_receiver() {
    assert_eq!(
        run(
            "val impl = 123\n\
             \n\
             object O {\n\
             \x20   val impl = 7\n\
             }\n\
             \n\
             operator fun Any?.getValue(thisRef: Any?, property: Any?) =\n\
             \x20   if (this == 123 || this == 7 || this == 1) \"ok\" else \"no\"\n\
             \n\
             val topLevel: String by impl\n\
             val viaObject: String by O.impl\n\
             val viaConst: String by 1\n\
             \n\
             class Holder {\n\
             \x20   val impl = 123\n\
             \x20   val member: String by impl\n\
             }\n\
             \n\
             fun box(): String {\n\
             \x20   if (topLevel != \"ok\") return \"FAIL1\"\n\
             \x20   if (viaObject != \"ok\") return \"FAIL2\"\n\
             \x20   if (viaConst != \"ok\") return \"FAIL3\"\n\
             \x20   if (Holder().member != \"ok\") return \"FAIL4\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "ScalarDelegate",
        ),
        "OK"
    );
}

/// An operator declared ON the scalar keeps its receiver unboxed — the boxing above must be driven
/// by the declared slot, not by the delegate being a scalar.
#[test]
fn a_scalar_receiver_the_operator_declares_stays_unboxed() {
    assert_eq!(
        run(
            "operator fun Int.getValue(thisRef: Any?, property: Any?) = this + 1\n\
             \n\
             val counted: Int by 41\n\
             \n\
             fun box(): String = if (counted == 42) \"OK\" else \"FAIL\"\n",
            "ScalarReceiverDelegate",
        ),
        "OK"
    );
}

/// The accessor's own return type against the operator's: an uncommon fixture-owned delegate keeps
/// the mechanism isolated from stdlib intrinsic handling. Its generic `getValue` returns erased
/// `T`, so a reference property needs the cast and a scalar one the unbox.
#[test]
fn a_delegated_accessor_returns_its_own_type() {
    assert_eq!(
        run(
            "class Coffer<T>(private val stored: T) {\n\
             \x20   operator fun getValue(thisRef: Any?, property: Any?): T = stored\n\
             }\n\
             \n\
             class User {\n\
             \x20   val name: String by Coffer(\"John\")\n\
             \x20   val age: Int by Coffer(25)\n\
             \x20   val score: Double by Coffer(1.5)\n\
             }\n\
             \n\
             fun box(): String {\n\
             \x20   val user = User()\n\
             \x20   if (user.name != \"John\") return \"FAIL1\"\n\
             \x20   if (user.age != 25) return \"FAIL2\"\n\
             \x20   if (user.score != 1.5) return \"FAIL3\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "CofferDelegate",
        ),
        "OK"
    );
}

/// A widening of the operator's result to the property's declared type, which is the same boundary
/// read the other way: `getValue` returns `Int` where the property is a `Number`.
#[test]
fn a_delegated_accessor_widens_the_operators_result() {
    assert_eq!(
        run(
            "class Delegate {\n\
             \x20   operator fun getValue(t: Any?, p: Any?): Int = 1\n\
             }\n\
             \n\
             class Holder {\n\
             \x20   val prop: Number by Delegate()\n\
             }\n\
             \n\
             fun box(): String = if (Holder().prop == 1) \"OK\" else \"FAIL\"\n",
            "WidenedDelegate",
        ),
        "OK"
    );
}

/// `javap -c` of one class as BOTH compilers emit it: `(kotlinc, krusty)`.
fn javap_both(stem: &str, source: &str, class: &str) -> (String, String) {
    let dir = common::scratch_dir().expect("scratch dir");
    let reference_dir = dir.join(format!("{stem}-ref"));
    let ours_dir = dir.join(format!("{stem}-out"));
    std::fs::create_dir_all(&reference_dir).expect("reference dir");
    std::fs::create_dir_all(&ours_dir).expect("output dir");
    let source_path = dir.join(format!("{stem}.kt"));
    std::fs::write(&source_path, source).expect("write source");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc unavailable under the test harness");
    assert_eq!(code, 0, "{stem}: kotlinc failed: {stderr}");
    let classes = common::compile_in_process_metadata_cp(source, stem, &[common::stdlib_jar()])
        .unwrap_or_else(|| panic!("{stem}: krusty failed to compile"));
    for (name, bytes) in &classes {
        let path = ours_dir.join(format!("{name}.class"));
        std::fs::create_dir_all(path.parent().expect("class parent")).expect("class dir");
        std::fs::write(&path, bytes).expect("write class");
    }
    let dump = |root: &std::path::Path| {
        common::javap(&["-p", "-c", "-cp", &root.to_string_lossy(), class])
            .unwrap_or_else(|| panic!("{stem}: javap failed"))
    };
    (dump(&reference_dir), dump(&ours_dir))
}

/// The instructions of the one method whose header contains `needle`, as
/// `<mnemonic> <symbolic operand>` so constant-pool indices stay out of the comparison.
fn body(dump: &str, needle: &str) -> Vec<String> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for line in dump.lines() {
        let trimmed = line.trim();
        // A member header is two-space indented and ends the declaration; `static {};` is one too.
        if line.starts_with("  ") && !line.starts_with("   ") && trimmed.ends_with(';') {
            out.push((trimmed.to_string(), Vec::new()));
            continue;
        }
        let Some((offset, rest)) = trimmed.split_once(": ") else {
            continue;
        };
        if offset.is_empty() || !offset.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Some((_, rows)) = out.last_mut() else {
            continue;
        };
        rows.push(match rest.split_once("// ") {
            Some((code, comment)) => format!(
                "{} {}",
                code.split_whitespace().next().unwrap_or(""),
                comment.trim()
            ),
            None => rest.split_whitespace().collect::<Vec<_>>().join(" "),
        });
    }
    let found: Vec<_> = out
        .into_iter()
        .filter(|(header, _)| header.contains(needle))
        .collect();
    assert_eq!(found.len(), 1, "exactly one method matching {needle:?}");
    found.into_iter().next().expect("method").1
}

/// A MEMBER `var` of scalar type over a generic delegate: the read unboxes the operator's erased
/// result, the write boxes the value into its erased parameter. Both compilers' ledgers are spelled
/// out — they differ only in how the `KProperty` is reached (kotlinc interns one
/// `$$delegatedProperties` array, krusty a field per property), which is a separate representation
/// difference and is visible here rather than normalised away.
#[test]
fn a_member_accessor_pair_crosses_the_boundary_like_kotlinc() {
    let source = "class PVar<T> {\n\
                  \x20   private var value: Any? = null\n\
                  \x20   operator fun getValue(thisRef: Any?, prop: Any?): T = value as T\n\
                  \x20   operator fun setValue(thisRef: Any?, prop: Any?, newValue: T) { value = newValue }\n\
                  }\n\
                  \n\
                  class Holder {\n\
                  \x20   var x: Long by PVar<Long>()\n\
                  }\n";
    let (reference, ours) = javap_both("DelegateMemberPair", source, "Holder");
    assert_eq!(
        body(&reference, "long getX()"),
        vec![
            "aload_0".to_string(),
            "getfield Field x$delegate:LPVar;".to_string(),
            "aload_0".to_string(),
            "getstatic Field $$delegatedProperties:[Lkotlin/reflect/KProperty;".to_string(),
            "iconst_0".to_string(),
            "aaload".to_string(),
            "invokevirtual Method PVar.getValue:(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;".to_string(),
            "checkcast class java/lang/Number".to_string(),
            "invokevirtual Method java/lang/Number.longValue:()J".to_string(),
            "lreturn".to_string(),
        ],
        "kotlinc's own getX ledger"
    );
    assert_eq!(
        body(&ours, "long getX()"),
        vec![
            "aload_0".to_string(),
            "getfield Field x$delegate:LPVar;".to_string(),
            "aload_0".to_string(),
            "getstatic Field x$kprop:Lkotlin/reflect/KProperty;".to_string(),
            "invokevirtual Method PVar.getValue:(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;".to_string(),
            "checkcast class java/lang/Long".to_string(),
            "invokevirtual Method java/lang/Long.longValue:()J".to_string(),
            "lreturn".to_string(),
        ],
        "krusty's getX ledger: the unbox is there; the KProperty carrier and its wrapper owner differ"
    );
    assert_eq!(
        body(&ours, "void setX(long)"),
        vec![
            "aload_0".to_string(),
            "getfield Field x$delegate:LPVar;".to_string(),
            "aload_0".to_string(),
            "getstatic Field x$kprop:Lkotlin/reflect/KProperty;".to_string(),
            "lload_1".to_string(),
            "invokestatic Method java/lang/Long.valueOf:(J)Ljava/lang/Long;".to_string(),
            "invokevirtual Method PVar.setValue:(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)V".to_string(),
            "return".to_string(),
        ],
        "krusty's setX ledger: the value is boxed into the erased parameter"
    );
    assert_eq!(
        body(&reference, "void setX(long)")
            .into_iter()
            .filter(|row| !row.contains("delegatedProperties")
                && row != "iconst_0"
                && row != "aaload")
            .collect::<Vec<_>>(),
        body(&ours, "void setX(long)")
            .into_iter()
            .filter(|row| !row.contains("$kprop"))
            .collect::<Vec<_>>(),
        "setX agrees with kotlinc once the KProperty carrier is set aside"
    );
}

/// A `provideDelegate` RECEIVER. The delegate EXPRESSION reaches the operator's erased receiver
/// slot, so a scalar one has to be boxed there too — `val byInt by 42` pushed a raw `int` into
/// `provideDelegate(Object, Object, KProperty)` and the class failed to verify. The adaptation fact
/// now comes from the checked delegate call itself, which is what removed the hole: the lowering no
/// longer depends on a caller knowing the receiver's type.
#[test]
fn a_provide_delegate_receiver_is_boxed() {
    let source = "class Casket<C, T>(private val stored: T) {\n\
                  \x20   operator fun getValue(thisRef: C, property: Any?): T = stored\n\
                  }\n\
                  \n\
                  operator fun <C, T> T.provideDelegate(thisRef: C, property: Any?) =\n\
                  \x20   Casket<C, T>(this)\n\
                  \n\
                  val byInt by 42\n\
                  val byIntNullable: Int? by 42\n\
                  val byString by \"str\"\n\
                  \n\
                  fun box(): String {\n\
                  \x20   if (byInt != 42) return \"FAIL1\"\n\
                  \x20   if (byIntNullable != 42) return \"FAIL2\"\n\
                  \x20   if (byString != \"str\") return \"FAIL3\"\n\
                  \x20   return \"OK\"\n\
                  }\n";
    assert_eq!(run(source, "ProvideDelegateReceiver"), "OK");

    let (_, ours) = javap_both(
        "ProvideDelegateReceiverDump",
        source,
        "ProvideDelegateReceiverDumpKt",
    );
    let clinit = body(&ours, "static {}");
    let boxed = clinit
        .windows(2)
        .filter(|pair| {
            pair[0] == "bipush 42"
                && pair[1] == "invokestatic Method java/lang/Integer.valueOf:(I)Ljava/lang/Integer;"
        })
        .count();
    assert_eq!(
        boxed, 2,
        "each scalar delegate expression is boxed: {clinit:?}"
    );
    // A REFERENCE delegate expression needs no adapter, so the boxing is not unconditional.
    assert!(
        clinit.iter().any(|row| row == "ldc String str"),
        "the String delegate reaches the receiver directly: {clinit:?}"
    );
}

/// The WRITTEN VALUE. The accessor takes the property's own type while the operator declares its
/// (erased) parameter, so `var x: Long by …` handed a raw `long` to
/// `setValue(Object, Object, Object)` and the class failed to verify:
/// `Type long_2nd … is not assignable to 'java/lang/Object'`.
///
/// The operator lives in ANOTHER FILE of the same module, which is the shape that was broken: a
/// same-file operator is realized as a method of this file's own IR and reached the write already
/// adapted (`a_member_accessor_pair_crosses_the_boundary_like_kotlinc` pins that ledger), so only
/// the cross-file form reached the call with a raw scalar. Reduced from box
/// `delegatedProperty/genericSetValueViaSyntheticAccessor.kt`, whose operator is `protected` and
/// inherited and whose delegate is an inner class's enclosing instance.
#[test]
fn a_written_value_is_boxed_for_an_erased_parameter() {
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&[
            (
                "Var.kt",
                "package pvar\n\
                 \n\
                 open class PVar<T> {\n\
                 \x20   private var slot: Any? = null\n\
                 \x20   @Suppress(\"UNCHECKED_CAST\")\n\
                 \x20   protected operator fun getValue(thisRef: Any?, prop: Any?): T = slot as T\n\
                 \x20   protected operator fun setValue(thisRef: Any?, prop: Any?, newValue: T) {\n\
                 \x20       slot = newValue\n\
                 \x20   }\n\
                 }\n",
            ),
            (
                "DelegatedWrittenValue.kt",
                "import pvar.*\n\
                 \n\
                 class C : PVar<Long>() {\n\
                 \x20   inner class Inner {\n\
                 \x20       var x by this@C\n\
                 \x20   }\n\
                 }\n\
                 \n\
                 fun box(): String {\n\
                 \x20   val inner = C().Inner()\n\
                 \x20   inner.x = 1L\n\
                 \x20   if (inner.x != 1L) return \"FAIL1\"\n\
                 \x20   return \"OK\"\n\
                 }\n",
            ),
        ])
        .expect("the source set must compile and run"),
        "OK"
    );
}

/// A `setValue` that declares the scalar itself keeps the written value unboxed — the adaptation
/// above is driven by the declared slot, not by the value being a scalar.
#[test]
fn a_written_value_the_operator_declares_stays_unboxed() {
    let source = "class LongCell {\n\
                  \x20   var slot: Long = 0L\n\
                  \x20   operator fun getValue(thisRef: Any?, prop: Any?): Long = slot\n\
                  \x20   operator fun setValue(thisRef: Any?, prop: Any?, newValue: Long) {\n\
                  \x20       slot = newValue\n\
                  \x20   }\n\
                  }\n\
                  \n\
                  class Holder {\n\
                  \x20   var x: Long by LongCell()\n\
                  }\n";
    let (_, ours) = javap_both("DelegatedScalarParameter", source, "Holder");
    assert_eq!(
        body(&ours, "void setX(long)"),
        vec![
            "aload_0".to_string(),
            "getfield Field x$delegate:LLongCell;".to_string(),
            "aload_0".to_string(),
            "getstatic Field x$kprop:Lkotlin/reflect/KProperty;".to_string(),
            "lload_1".to_string(),
            "invokevirtual Method LongCell.setValue:(Ljava/lang/Object;Ljava/lang/Object;J)V"
                .to_string(),
            "return".to_string(),
        ],
        "a declared scalar parameter takes the value directly"
    );
}

/// A generic `getValue`/`setValue` pair, used by the ledger cases below. Its operators declare the
/// class's own `T`, which erases to `Object`, so every property type over it has a boundary.
const CELL: &str = "class Cell<T> {\n\
                    \x20   private var slot: Any? = null\n\
                    \x20   @Suppress(\"UNCHECKED_CAST\")\n\
                    \x20   operator fun getValue(thisRef: Any?, prop: Any?): T = slot as T\n\
                    \x20   operator fun setValue(thisRef: Any?, prop: Any?, newValue: T) {\n\
                    \x20       slot = newValue\n\
                    \x20   }\n\
                    }\n\n";

/// Drop how the `KProperty` is reached, which is a separate representation difference: kotlinc
/// interns one `$$delegatedProperties` array (a `getstatic`, an index push and an `aaload`), krusty
/// a static field per property (one `getstatic`). What is left is the adaptation ledger.
fn without_kproperty_carrier(rows: Vec<String>) -> Vec<String> {
    let indexed_load = |position: usize| {
        rows[position].starts_with("iconst_")
            && rows.get(position + 1).is_some_and(|next| next == "aaload")
    };
    rows.iter()
        .enumerate()
        .filter(|(position, row)| {
            !row.contains("delegatedProperties")
                && !row.contains("$kprop")
                && row.as_str() != "aaload"
                && !indexed_load(*position)
        })
        .map(|(_, row)| row.clone())
        .collect()
}

/// A MEMBER EXTENSION delegate: `var Int.ext: Long by …` has TWO receivers reaching the operator,
/// and both cross the boundary — the extension receiver as the operator's `thisRef` argument, the
/// written value as its `newValue`. The whole ledger agrees with kotlinc.
#[test]
fn a_member_extension_delegate_crosses_both_receivers_like_kotlinc() {
    let source = format!(
        "{CELL}class Holder {{\n\
         \x20   var Int.ext: Long by Cell<Long>()\n\
         }}\n"
    );
    let (reference, ours) = javap_both("DelegateMemberExtension", &source, "Holder");
    assert_eq!(
        without_kproperty_carrier(body(&ours, "long getExt(int)")),
        vec![
            "aload_0".to_string(),
            "getfield Field ext$delegate:LCell;".to_string(),
            "iload_1".to_string(),
            "invokestatic Method java/lang/Integer.valueOf:(I)Ljava/lang/Integer;".to_string(),
            "invokevirtual Method Cell.getValue:(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;".to_string(),
            "checkcast class java/lang/Long".to_string(),
            "invokevirtual Method java/lang/Long.longValue:()J".to_string(),
            "lreturn".to_string(),
        ],
        "the scalar extension receiver is boxed into the operator's thisRef slot"
    );
    assert_eq!(
        without_kproperty_carrier(body(&ours, "void setExt(int, long)")),
        vec![
            "aload_0".to_string(),
            "getfield Field ext$delegate:LCell;".to_string(),
            "iload_1".to_string(),
            "invokestatic Method java/lang/Integer.valueOf:(I)Ljava/lang/Integer;".to_string(),
            "lload_2".to_string(),
            "invokestatic Method java/lang/Long.valueOf:(J)Ljava/lang/Long;".to_string(),
            "invokevirtual Method Cell.setValue:(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)V".to_string(),
            "return".to_string(),
        ],
        "both the extension receiver and the written value are boxed"
    );
    // kotlinc's own ledgers, with the one wrapper-owner difference already recorded in SPEC.md
    // (`checkcast Number` where krusty casts to the exact wrapper) normalised away.
    let exact_wrapper = |rows: Vec<String>| {
        rows.into_iter()
            .map(|row| row.replace("class java/lang/Number", "class java/lang/Long"))
            .map(|row| row.replace("java/lang/Number.longValue", "java/lang/Long.longValue"))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        exact_wrapper(without_kproperty_carrier(body(
            &reference,
            "long getExt(int)"
        ))),
        without_kproperty_carrier(body(&ours, "long getExt(int)")),
        "getExt agrees with kotlinc"
    );
    assert_eq!(
        without_kproperty_carrier(body(&reference, "void setExt(int, long)")),
        without_kproperty_carrier(body(&ours, "void setExt(int, long)")),
        "setExt agrees with kotlinc"
    );
}

/// A NULLABLE scalar carrier is a reference, so it crosses no representation boundary: the write
/// passes it straight through and the read only narrows. This is the case a rule phrased as "box a
/// scalar property" would get wrong, and it agrees with kotlinc instruction for instruction.
#[test]
fn a_nullable_scalar_carrier_is_passed_through() {
    let source = format!(
        "{CELL}class Holder {{\n\
         \x20   var x: Int? by Cell<Int?>()\n\
         }}\n"
    );
    let (reference, ours) = javap_both("DelegateNullableCarrier", &source, "Holder");
    assert_eq!(
        without_kproperty_carrier(body(&ours, "java.lang.Integer getX()")),
        vec![
            "aload_0".to_string(),
            "getfield Field x$delegate:LCell;".to_string(),
            "aload_0".to_string(),
            "invokevirtual Method Cell.getValue:(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;".to_string(),
            "checkcast class java/lang/Integer".to_string(),
            "areturn".to_string(),
        ],
        "a nullable read narrows and does not unbox"
    );
    assert_eq!(
        without_kproperty_carrier(body(&ours, "void setX(java.lang.Integer)")),
        vec![
            "aload_0".to_string(),
            "getfield Field x$delegate:LCell;".to_string(),
            "aload_0".to_string(),
            "aload_1".to_string(),
            "invokevirtual Method Cell.setValue:(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)V".to_string(),
            "return".to_string(),
        ],
        "a nullable write carries no adapter at all"
    );
    for member in ["java.lang.Integer getX()", "void setX(java.lang.Integer)"] {
        assert_eq!(
            without_kproperty_carrier(body(&reference, member)),
            without_kproperty_carrier(body(&ours, member)),
            "{member} agrees with kotlinc"
        );
    }
}

/// VALUE CLASSES over both carrier kinds. The boundary is the same one, and the instructions it
/// costs are the value class's own `box-impl`/`unbox-impl` rather than a wrapper's — a fact the
/// backend derives from the physical types, which is why the lowering states only that the two
/// semantic types differ. Both ledgers are kotlinc's, instruction for instruction.
#[test]
fn a_value_class_carrier_crosses_the_boundary_like_kotlinc() {
    let source = format!(
        "@JvmInline value class Id(val raw: Int)\n\
         @JvmInline value class Name(val raw: String)\n\n\
         {CELL}class Holder {{\n\
         \x20   var id: Id by Cell<Id>()\n\
         \x20   var name: Name by Cell<Name>()\n\
         }}\n"
    );
    let (reference, ours) = javap_both("DelegateValueClassCarrier", &source, "Holder");
    assert_eq!(
        without_kproperty_carrier(body(&ours, "int getId-")),
        vec![
            "aload_0".to_string(),
            "getfield Field id$delegate:LCell;".to_string(),
            "aload_0".to_string(),
            "invokevirtual Method Cell.getValue:(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;".to_string(),
            "checkcast class Id".to_string(),
            "invokevirtual Method Id.\"unbox-impl\":()I".to_string(),
            "ireturn".to_string(),
        ],
        "a scalar-carrier value class unboxes through its own accessor"
    );
    assert_eq!(
        without_kproperty_carrier(body(&ours, "java.lang.String getName-")),
        vec![
            "aload_0".to_string(),
            "getfield Field name$delegate:LCell;".to_string(),
            "aload_0".to_string(),
            "invokevirtual Method Cell.getValue:(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;".to_string(),
            "checkcast class Name".to_string(),
            "invokevirtual Method Name.\"unbox-impl\":()Ljava/lang/String;".to_string(),
            "areturn".to_string(),
        ],
        "a REFERENCE-carrier value class is unboxed too — it is not a scalar boundary"
    );
    for member in ["int getId-", "void setId-", "java.lang.String getName-"] {
        assert_eq!(
            without_kproperty_carrier(body(&reference, member)),
            without_kproperty_carrier(body(&ours, member)),
            "{member} agrees with kotlinc"
        );
    }
    // kotlinc additionally null-checks the non-null `String` setter parameter; that parameter check
    // is a separate gap, so the boxing is compared with it set aside.
    assert_eq!(
        {
            let reference = without_kproperty_carrier(body(&reference, "void setName-"));
            let check = reference
                .iter()
                .position(|row| row.contains("checkNotNullParameter"))
                .expect("kotlinc null-checks a non-null setter parameter");
            reference[check + 1..].to_vec()
        },
        without_kproperty_carrier(body(&ours, "void setName-")),
        "setName agrees with kotlinc once the parameter null-check is set aside"
    );
}

/// The same boundary against a CROSS-MODULE operator: the delegate class is built separately and
/// consumed from the classpath, so the call is realized as an external target rather than a method
/// of this module. Compiled and RUN over a scalar, a nullable carrier and a value class.
#[test]
fn a_classpath_operator_crosses_the_boundary_too() {
    let library = "package cell\n\
                   \n\
                   class Cell<T>(private var slot: Any?) {\n\
                   \x20   @Suppress(\"UNCHECKED_CAST\")\n\
                   \x20   operator fun getValue(thisRef: Any?, prop: Any?): T = slot as T\n\
                   \x20   operator fun setValue(thisRef: Any?, prop: Any?, newValue: T) {\n\
                   \x20       slot = newValue\n\
                   \x20   }\n\
                   }\n";
    let main = "import cell.Cell\n\
                \n\
                @JvmInline value class Id(val raw: Int)\n\
                \n\
                class Holder {\n\
                \x20   var scalar: Long by Cell<Long>(0L)\n\
                \x20   var nullable: Int? by Cell<Int?>(null)\n\
                \x20   var id: Id by Cell<Id>(Id(0))\n\
                }\n\
                \n\
                fun box(): String {\n\
                \x20   val holder = Holder()\n\
                \x20   holder.scalar = 7L\n\
                \x20   if (holder.scalar != 7L) return \"FAIL1\"\n\
                \x20   holder.nullable = 3\n\
                \x20   if (holder.nullable != 3) return \"FAIL2\"\n\
                \x20   holder.id = Id(9)\n\
                \x20   if (holder.id.raw != 9) return \"FAIL3\"\n\
                \x20   return \"OK\"\n\
                }\n";
    assert_eq!(
        common::run_box_against("delegate_scalar_boundary_classpath", library, main).as_deref(),
        Some("OK")
    );
}

/// A classpath MEMBER-EXTENSION has two receivers: the dispatch object remains the external call's
/// receiver, while the stored delegate is inserted into the extension-receiver parameter slot.
/// The latter still crosses the selected declaration boundary and must not bypass its adaptation.
#[test]
fn a_classpath_member_extension_adapts_its_delegate_receiver() {
    let library = "package rules\n\
                   \n\
                   open class Rules {\n\
                   \x20   operator fun <T> T.getValue(thisRef: Any?, prop: Any?): T = this\n\
                   }\n";
    let main = "import rules.Rules\n\
                \n\
                class Holder : Rules() {\n\
                \x20   val answer: Int by 42\n\
                }\n\
                \n\
                fun box(): String = if (Holder().answer == 42) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        common::run_box_against("delegate_member_extension_classpath", library, main).as_deref(),
        Some("OK")
    );
}

/// kotlinc's COMPLETE ordered error ledger for one source, as `line:col: error: message`.
///
/// A kotlinc diagnostic is not one line: after its header come the message's own continuation lines
/// (the candidate list of "None of the following functions is applicable:"), then an echo of the
/// offending source line, then a caret ruler. The echo is that source line verbatim, which is what
/// separates it from the continuations — so the ledger keeps the whole message and drops only
/// kotlinc's rendering of where it points, which the header already states exactly.
fn kotlinc_error_ledger(source: &str, file_name: &str, out_dir: &std::path::Path) -> Vec<String> {
    let dir = common::scratch_dir().expect("scratch dir");
    let source_path = dir.join(file_name);
    std::fs::write(&source_path, source).expect("write source");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out_dir.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc unavailable under the test harness");
    assert_ne!(code, 0, "kotlinc must reject this source");
    let source_lines = source.lines().collect::<Vec<_>>();
    let mut ledger: Vec<String> = Vec::new();
    // `Some(line)` while a diagnostic is open and its source echo has not been reached yet.
    let mut pending_echo: Option<usize> = None;
    for line in stderr.lines() {
        // kotlinc names the file by the path it was handed, which is the scratch directory's.
        let header = line
            .rsplit('/')
            .next()
            .unwrap_or(line)
            .strip_prefix(&format!("{file_name}:"))
            .and_then(|rest| {
                let (position, message) = rest.split_once(": ")?;
                let (line_no, column) = position.split_once(':')?;
                let message = message.strip_prefix("error: ")?;
                Some((
                    line_no.parse::<usize>().ok()?,
                    column.to_string(),
                    message.to_string(),
                ))
            });
        if let Some((line_no, column, message)) = header {
            ledger.push(format!("{line_no}:{column}: error: {message}"));
            pending_echo = Some(line_no);
            continue;
        }
        let Some(line_no) = pending_echo else {
            continue;
        };
        if line == *source_lines.get(line_no - 1).unwrap_or(&"") {
            pending_echo = None;
            continue;
        }
        let last = ledger
            .last_mut()
            .expect("a continuation follows its header");
        last.push('\n');
        last.push_str(line);
    }
    ledger
}

/// A CONTEXT-PREFIXED convention operator is not a delegate convention at all. The operator is
/// called from a GENERATED accessor, which has no scope to fill an implicit context from, so
/// kotlinc rejects the declaration and then reports the property as having no APPLICABLE
/// convention. krusty rejects the declaration with the same message, from the same rule that
/// excludes the candidate during selection — which is why the checked plan's declared slots are
/// always the operator's own, with no context prefix for lowering to find the end of.
///
/// The whole ledger matches kotlinc's: four diagnostics, same order, same positions, same messages
/// down to the rendering of the excluded candidates.
#[test]
fn a_context_prefixed_operator_is_rejected_on_its_declaration() {
    let source = "class Salt(val value: Long)\n\
                  \n\
                  class Cell<T> {\n\
                  \x20   private var slot: Any? = null\n\
                  \x20   @Suppress(\"UNCHECKED_CAST\")\n\
                  \x20   context(salt: Salt)\n\
                  \x20   operator fun getValue(thisRef: Any?, prop: Any?): T = slot as T\n\
                  \x20   context(salt: Salt)\n\
                  \x20   operator fun setValue(thisRef: Any?, prop: Any?, newValue: T) {\n\
                  \x20       slot = newValue\n\
                  \x20   }\n\
                  }\n\
                  \n\
                  class Holder {\n\
                  \x20   var x: Long by Cell<Long>()\n\
                  }\n\
                  \n\
                  fun box(): String = \"OK\"\n";

    let dir = common::scratch_dir().expect("scratch dir");
    let expected = vec![
        "6:5: error: context parameters on delegation operators are unsupported.".to_string(),
        "8:5: error: context parameters on delegation operators are unsupported.".to_string(),
        "15:17: error: property delegate must have a \'getValue(Holder, \
         KMutableProperty1<Holder, Long>)\' method. None of the following functions is \
         applicable:\ncontext(salt: Salt) fun getValue(thisRef: Any?, prop: Any?): Long"
            .to_string(),
        "15:17: error: property delegate must have a \'setValue(Holder, \
         KMutableProperty1<Holder, Long>, Long)\' method. None of the following functions is \
         applicable:\ncontext(salt: Salt) fun setValue(thisRef: Any?, prop: Any?, newValue: Long): \
         Unit"
            .to_string(),
    ];
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "ContextPrefixedOperator.kt",
            &dir.join("ContextPrefixedOperator-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger"
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty reports the same ledger, entry for entry"
    );
}

/// A member-extension convention is subject to the same generated-accessor rule as a convention
/// declared on the delegate itself. Selection must retain the raw declaration even when its
/// context cannot be instantiated from the lexical receiver tower, so the refusal still lists it.
#[test]
fn an_unsatisfied_context_member_extension_is_still_a_delegate_candidate() {
    let source = "class MissingContext\n\
                  class Cell\n\
                  \n\
                  class Holder {\n\
                  \x20   context(missing: MissingContext)\n\
                  \x20   operator fun Cell.getValue(thisRef: Holder, prop: Any?): Long = 1\n\
                  \x20   val inferred by Cell()\n\
                  }\n\
                  \n\
                  fun unrelated() {\n\
                  \x20   val mismatched: Int = \"not an Int\"\n\
                  }\n";
    let expected = vec![
        "5:5: error: context parameters on delegation operators are unsupported.".to_string(),
        "7:18: error: property delegate must have a 'getValue(Holder, KProperty1<Holder, Long>)' method. None of the following functions is applicable:\ncontext(missing: MissingContext) fun Cell.getValue(thisRef: Holder, prop: Any?): Long"
            .to_string(),
        "11:25: error: initializer type mismatch: expected 'Int', actual 'String'.".to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "ContextMemberExtensionDelegate.kt",
            &dir.join("ContextMemberExtensionDelegate-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// An unusable member extension is diagnostic inventory, not a terminal lookup rung. Both
/// signature inference and checked-body selection must continue to the valid ordinary extension;
/// the inferred `Long` return makes that choice observable without weakening the exact ledger.
#[test]
fn an_invalid_member_extension_does_not_displace_a_valid_ordinary_extension() {
    let source = "class MissingContext\n\
                  class Cell\n\
                  \n\
                  operator fun Cell.getValue(thisRef: Holder, prop: Any?): Long = 1\n\
                  \n\
                  class Holder {\n\
                  \x20   context(missing: MissingContext)\n\
                  \x20   operator fun Cell.getValue(thisRef: Holder, prop: Any?): String = \"wrong\"\n\
                  \x20   val inferred by Cell()\n\
                  }\n\
                  \n\
                  fun use(): Long = Holder().inferred\n\
                  \n\
                  fun unrelated() {\n\
                  \x20   val mismatched: Int = \"not an Int\"\n\
                  }\n";
    let expected = vec![
        "7:5: error: context parameters on delegation operators are unsupported.".to_string(),
        "15:25: error: initializer type mismatch: expected 'Int', actual 'String'.".to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "InvalidMemberExtensionValidOrdinaryDelegate.kt",
            &dir.join("InvalidMemberExtensionValidOrdinaryDelegate-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// A delegate that supplies NO convention at all — the ordinary mistake of writing `by` in front of
/// a type that was never a delegate. kotlinc names the method it looked for, and it names it once
/// per convention the property needs, so a `var` is told about both `getValue` and `setValue`.
///
/// Every receiver shape gets its own property-reference flavour, which is the whole content of the
/// message's second slot: no receiver is `KProperty0`, a member or an extension is `KProperty1`, a
/// member extension is `KProperty2`, and a `var` makes each one `KMutable…`. The `thisRef` slot
/// follows the same shape — `Nothing?` where the accessor passes null.
#[test]
fn a_delegate_with_no_convention_names_every_method_it_lacks() {
    let source = "class Plain\n\
                  \n\
                  class Holder {\n\
                  \x20   var member: Long by Plain()\n\
                  \x20   val readOnly: Long by Plain()\n\
                  \x20   var String.memberExtension: Long by Plain()\n\
                  }\n\
                  \n\
                  val topLevel: Long by Plain()\n\
                  var String.extension: Long by Plain()\n\
                  \n\
                  fun local() {\n\
                  \x20   val read: Long by Plain()\n\
                  \x20   var write: Long by Plain()\n\
                  \x20   write = read\n\
                  }\n";

    let dir = common::scratch_dir().expect("scratch dir");
    let expected = vec![
        "4:22: error: type \'Plain\' has no method \'getValue(Holder, KMutableProperty1<*, *>)\', \
         so it cannot serve as a delegate."
            .to_string(),
        "4:22: error: type \'Plain\' has no method \'setValue(Holder, KMutableProperty1<*, *>, \
         Long)\', so it cannot serve as a delegate for var (read-write property)."
            .to_string(),
        "5:24: error: type \'Plain\' has no method \'getValue(Holder, KProperty1<*, *>)\', so it \
         cannot serve as a delegate."
            .to_string(),
        "6:38: error: type \'Plain\' has no method \'getValue(String, KMutableProperty2<*, *, \
         *>)\', so it cannot serve as a delegate."
            .to_string(),
        "6:38: error: type \'Plain\' has no method \'setValue(String, KMutableProperty2<*, *, *>, \
         Long)\', so it cannot serve as a delegate for var (read-write property)."
            .to_string(),
        "9:20: error: type \'Plain\' has no method \'getValue(Nothing?, KProperty0<*>)\', so it \
         cannot serve as a delegate."
            .to_string(),
        "10:28: error: type \'Plain\' has no method \'getValue(String, KMutableProperty1<*, *>)\', \
         so it cannot serve as a delegate."
            .to_string(),
        "10:28: error: type \'Plain\' has no method \'setValue(String, KMutableProperty1<*, *>, \
         Long)\', so it cannot serve as a delegate for var (read-write property)."
            .to_string(),
        "13:20: error: type \'Plain\' has no method \'getValue(Nothing?, KProperty0<*>)\', so it \
         cannot serve as a delegate."
            .to_string(),
        "14:21: error: type \'Plain\' has no method \'getValue(Nothing?, \
         KMutableProperty0<*>)\', so it cannot serve as a delegate."
            .to_string(),
        "14:21: error: type \'Plain\' has no method \'setValue(Nothing?, KMutableProperty0<*>, \
         Long)\', so it cannot serve as a delegate for var (read-write property)."
            .to_string(),
    ];
    assert_eq!(
        kotlinc_error_ledger(source, "NoConvention.kt", &dir.join("NoConvention-ref")),
        expected,
        "kotlinc's complete ordered ledger"
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty reports the same ledger, entry for entry"
    );
}

/// A delegated property with NO declared type and no `getValue` on its delegate: the one shape
/// whose type has nothing to be inferred FROM once the convention is missing, so its signature
/// cannot finalize at all.
///
/// The declaration is still named, and the file's independent diagnostics still come out. Both
/// complete ordered ledgers are asserted, so the ONE entry krusty does not reproduce is pinned by
/// measurement rather than described: kotlinc cascades a second `setValue` refusal whose value slot
/// renders the failed inference itself (`??? (Unresolved name: getValue)`). krusty suppresses every
/// delegate-convention message whose operand types are already errors, because repeating a failure
/// under a second heading is what that suppression exists to prevent.
#[test]
fn an_untyped_delegated_property_names_its_missing_convention_and_the_rest_of_the_file() {
    let dir = common::scratch_dir().expect("scratch dir");
    let untyped = "class Plain\n\
                   \n\
                   var untyped by Plain()\n\
                   \n\
                   fun unrelated() {\n\
                   \x20   val mismatched: Int = \"not an Int\"\n\
                   \x20   println(mismatched)\n\
                   }\n";
    let missing_getvalue = "3:13: error: type \'Plain\' has no method \'getValue(Nothing?, \
                            KMutableProperty0<*>)\', so it cannot serve as a delegate."
        .to_string();
    let unrelated_mismatch =
        "6:25: error: initializer type mismatch: expected \'Int\', actual \'String\'.".to_string();
    assert_eq!(
        kotlinc_error_ledger(untyped, "Untyped.kt", &dir.join("Untyped-ref")),
        vec![
            missing_getvalue.clone(),
            "3:13: error: type \'Plain\' has no method \'setValue(Nothing?, KMutableProperty0<*>, \
             ??? (Unresolved name: getValue))\', so it cannot serve as a delegate for var \
             (read-write property)."
                .to_string(),
            unrelated_mismatch.clone(),
        ],
        "kotlinc\'s complete ordered ledger"
    );
    assert_eq!(
        common::front_end_diagnostics_located(untyped, &[common::stdlib_jar()], None),
        vec![missing_getvalue, unrelated_mismatch],
        "krusty\'s complete ordered ledger: the same entries without kotlinc\'s cascaded setValue"
    );

    // The same delegate, the same missing convention, declared as a local: reported exactly.
    let local = "class Plain\n\
                 \n\
                 fun local() {\n\
                 \x20   val untyped by Plain()\n\
                 }\n";
    let local_expected = vec![
        "4:17: error: type 'Plain' has no method 'getValue(Nothing?, KProperty0<*>)', so it cannot serve as a delegate."
            .to_string(),
    ];
    assert_eq!(
        kotlinc_error_ledger(local, "UntypedLocal.kt", &dir.join("UntypedLocal-ref")),
        local_expected,
        "kotlinc's complete ordered local ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(local, &[common::stdlib_jar()], None),
        local_expected,
        "krusty's complete ordered local ledger",
    );
}

/// A statement-local delegate can participate in an enclosing inferred signature. Its compact
/// Pass-1 node owns the local site's mutability and exact `by` origin; the enclosing function is
/// only the diagnostic owner and must never be mistaken for the delegated declaration.
#[test]
fn an_inferred_statement_local_delegate_reports_from_its_pass_one_site() {
    let source = "class Plain\n\
                  \n\
                  fun inferred() = ({\n\
                  \x20   val local by Plain()\n\
                  \x20   local\n\
                  })()\n\
                  \n\
                  fun unrelated() {\n\
                  \x20   val mismatched: Int = \"not an Int\"\n\
                  }\n";
    let expected = vec![
        "4:15: error: type 'Plain' has no method 'getValue(Nothing?, KProperty0<*>)', so it cannot serve as a delegate."
            .to_string(),
        "9:25: error: initializer type mismatch: expected 'Int', actual 'String'.".to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(source, "PassOneLocal.kt", &dir.join("PassOneLocal-ref")),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// A member of a local classifier is still a MEMBER convention site. Treating every declaration
/// reached through a local body as statement-local passes `null`; the real dispatch instance must
/// instead appear both as `thisRef` and in `KProperty1`.
#[test]
fn an_inferred_local_class_member_delegate_keeps_its_dispatch_site() {
    let source = "class DispatchProbe {\n\
                  \x20   operator fun getValue(owner: Nothing?, property: Any?): Int = 1\n\
                  }\n\
                  \n\
                  fun inferred() = ({\n\
                  \x20   class LocalOwner {\n\
                  \x20       val member by DispatchProbe()\n\
                  \x20   }\n\
                  \x20   LocalOwner().member\n\
                  })()\n";
    let expected = vec![
        "7:20: error: property delegate must have a 'getValue(LocalOwner, KProperty1<LocalOwner, Int>)' method. None of the following functions is applicable:\nfun getValue(owner: Nothing?, property: Any?): Int"
            .to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "LocalClassDispatch.kt",
            &dir.join("LocalClassDispatch-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// A top-level extension passes null to `provideDelegate`, selecting the broken result below, while
/// its getter receives the extension `String`. Reusing that `String` for provide would select the
/// good result and incorrectly erase the missing-getValue refusal.
#[test]
fn an_inferred_top_level_extension_uses_distinct_delegate_first_arguments() {
    let source = "class GoodTopLevelDelegate {\n\
                  \x20   operator fun getValue(owner: String, property: Any?): Int = 1\n\
                  }\n\
                  class BrokenTopLevelDelegate\n\
                  \n\
                  class TopLevelExtensionFactory {\n\
                  \x20   operator fun provideDelegate(owner: Nothing?, property: Any?): BrokenTopLevelDelegate = BrokenTopLevelDelegate()\n\
                  \x20   operator fun provideDelegate(owner: String, property: Any?): GoodTopLevelDelegate = GoodTopLevelDelegate()\n\
                  }\n\
                  \n\
                  val String.inferred by TopLevelExtensionFactory()\n\
                  \n\
                  fun unrelated() {\n\
                  \x20   val mismatched: Int = \"not an Int\"\n\
                  }\n";
    let expected = vec![
        "11:21: error: type 'BrokenTopLevelDelegate' has no method 'getValue(String, KProperty1<*, *>)', so it cannot serve as a delegate."
            .to_string(),
        "14:25: error: initializer type mismatch: expected 'Int', actual 'String'.".to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "TopLevelExtensionDelegateReceivers.kt",
            &dir.join("TopLevelExtensionDelegateReceivers-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// A member extension passes its dispatch owner to `provideDelegate`, selecting the broken result,
/// and its extension value to `getValue`. Reusing the `String` for provide would select the good
/// result and incorrectly erase the missing-getValue refusal.
#[test]
fn an_inferred_member_extension_uses_distinct_delegate_first_arguments() {
    let source = "class GoodMemberExtensionDelegate {\n\
                  \x20   operator fun getValue(owner: String, property: Any?): Int = 1\n\
                  }\n\
                  class BrokenMemberExtensionDelegate\n\
                  \n\
                  class MemberExtensionFactory {\n\
                  \x20   operator fun provideDelegate(owner: MemberOwner, property: Any?): BrokenMemberExtensionDelegate = BrokenMemberExtensionDelegate()\n\
                  \x20   operator fun provideDelegate(owner: String, property: Any?): GoodMemberExtensionDelegate = GoodMemberExtensionDelegate()\n\
                  }\n\
                  \n\
                  class MemberOwner {\n\
                  \x20   val String.inferred by MemberExtensionFactory()\n\
                  }\n\
                  \n\
                  fun unrelated() {\n\
                  \x20   val mismatched: Int = \"not an Int\"\n\
                  }\n";
    let expected = vec![
        "12:25: error: type 'BrokenMemberExtensionDelegate' has no method 'getValue(String, KProperty2<*, *, *>)', so it cannot serve as a delegate."
            .to_string(),
        "16:25: error: initializer type mismatch: expected 'Int', actual 'String'.".to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "MemberExtensionDelegateReceivers.kt",
            &dir.join("MemberExtensionDelegateReceivers-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// These mandatory `getValue` member extensions are visible only through the property's implicit
/// dispatch receiver. Their equally applicable first arguments force a real Kotlin ambiguity,
/// which must retain both declarations for the shared convention diagnostic.
#[test]
fn an_inferred_delegate_reports_ambiguous_member_extension_get_value() {
    let source = "class MemberExtensionDelegate {\n\
                  }\n\
                  interface LeftOwner\n\
                  interface RightOwner\n\
                  \n\
                  class ExtensionScope : LeftOwner, RightOwner {\n\
                  \x20   operator fun MemberExtensionDelegate.getValue(owner: LeftOwner, property: Any?): Int = 1\n\
                  \x20   operator fun MemberExtensionDelegate.getValue(owner: RightOwner, property: Any?): Int = 2\n\
                  \x20   val inferred by MemberExtensionDelegate()\n\
                  }\n\
                  \n\
                  fun unrelated() {\n\
                  \x20   val mismatched: Int = \"not an Int\"\n\
                  }\n";
    let expected = vec![
        "9:18: error: overload resolution ambiguity on method 'getValue(ExtensionScope, KProperty1<*, *>)':\nfun MemberExtensionDelegate.getValue(owner: LeftOwner, property: Any?): Int\nfun MemberExtensionDelegate.getValue(owner: RightOwner, property: Any?): Int"
            .to_string(),
        "13:25: error: initializer type mismatch: expected 'Int', actual 'String'.".to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "AmbiguousMemberExtensionGetValue.kt",
            &dir.join("AmbiguousMemberExtensionGetValue-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// Ambiguous mandatory `getValue` selection is a source refusal owned by the `by` operation, and
/// an unrelated body error proves the rest of the file is still checked after signature recovery.
#[test]
fn ambiguous_get_value_reports_before_unrelated_body_errors() {
    let source = "class AmbiguousFactory {\n\
                  \x20   operator fun getValue(owner: String?, property: Any?): Int = 1\n\
                  \x20   operator fun getValue(owner: Int?, property: Any?): Int = 2\n\
                  \x20   context(irrelevantContext: String)\n\
                  \x20   fun getValue(owner: Any?, property: Any?, irrelevant: Int): String = \"ignored\"\n\
                  }\n\
                  \n\
                  val ambiguous by AmbiguousFactory()\n\
                  \n\
                  fun unrelated() {\n\
                  \x20   val mismatched: Int = \"not an Int\"\n\
                  }\n";
    let expected = vec![
        "8:15: error: overload resolution ambiguity on method 'getValue(Nothing?, KProperty0<*>)':\nfun getValue(owner: String?, property: Any?): Int\nfun getValue(owner: Int?, property: Any?): Int"
            .to_string(),
        "11:25: error: initializer type mismatch: expected 'Int', actual 'String'.".to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "AmbiguousGetValue.kt",
            &dir.join("AmbiguousGetValue-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// `provideDelegate` is optional: a genuinely ambiguous provide family is treated as absence, and
/// the mandatory `getValue` on the original delegate still determines the inferred property type.
#[test]
fn ambiguous_optional_provide_delegate_falls_through_to_get_value() {
    let source = "class OptionalFactory {\n\
                  \x20   operator fun provideDelegate(owner: String?, property: Any?): OptionalFactory = this\n\
                  \x20   operator fun provideDelegate(owner: Int?, property: Any?): OptionalFactory = this\n\
                  \x20   operator fun getValue(owner: Any?, property: Any?): Long = 1\n\
                  }\n\
                  \n\
                  val accepted by OptionalFactory()\n\
                  fun use(): Long = accepted\n\
                  \n\
                  fun unrelated() {\n\
                  \x20   val mismatched: Int = \"not an Int\"\n\
                  }\n";
    let expected = vec![
        "11:25: error: initializer type mismatch: expected 'Int', actual 'String'.".to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "AmbiguousOptionalProvide.kt",
            &dir.join("AmbiguousOptionalProvide-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// Signature inference uses the same convention tower as checked-body selection. A declaration on
/// the delegate wins first; without one, a member extension on the property's dispatch receiver
/// wins before an ordinary extension in file scope. Distinct results make both rungs observable.
#[test]
fn inferred_get_value_uses_member_then_member_extension_then_ordinary_extension_order() {
    let source = "class MemberCell {\n\
                  \x20   operator fun getValue(owner: Holder, property: Any?): String = \"member\"\n\
                  }\n\
                  class ExtensionCell\n\
                  \n\
                  operator fun MemberCell.getValue(owner: Holder, property: Any?): Long = 1\n\
                  operator fun ExtensionCell.getValue(owner: Holder, property: Any?): Long = 1\n\
                  \n\
                  class Holder {\n\
                  \x20   operator fun MemberCell.getValue(owner: Holder, property: Any?): Int = 1\n\
                  \x20   operator fun ExtensionCell.getValue(owner: Holder, property: Any?): Int = 1\n\
                  \x20   val memberFirst by MemberCell()\n\
                  \x20   val memberExtensionFirst by ExtensionCell()\n\
                  }\n\
                  \n\
                  fun memberUse(): String = Holder().memberFirst\n\
                  fun memberExtensionUse(): Int = Holder().memberExtensionFirst\n\
                  \n\
                  fun unrelated() {\n\
                  \x20   val mismatched: Int = \"not an Int\"\n\
                  }\n";
    let expected = vec![
        "20:25: error: initializer type mismatch: expected 'Int', actual 'String'.".to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "DelegateGetTower.kt",
            &dir.join("DelegateGetTower-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// `provideDelegate` follows that same three-rung tower before `getValue` observes the selected
/// storage delegate. The two factories deliberately produce delegates with different result types.
#[test]
fn inferred_provide_delegate_uses_member_then_member_extension_then_ordinary_extension_order() {
    let source = "class StringDelegate { operator fun getValue(owner: Holder, property: Any?): String = \"member\" }\n\
                  class IntDelegate { operator fun getValue(owner: Holder, property: Any?): Int = 1 }\n\
                  class LongDelegate { operator fun getValue(owner: Holder, property: Any?): Long = 1 }\n\
                  \n\
                  class MemberFactory {\n\
                  \x20   operator fun provideDelegate(owner: Holder, property: Any?): StringDelegate = StringDelegate()\n\
                  }\n\
                  class ExtensionFactory\n\
                  \n\
                  operator fun MemberFactory.provideDelegate(owner: Holder, property: Any?): LongDelegate = LongDelegate()\n\
                  operator fun ExtensionFactory.provideDelegate(owner: Holder, property: Any?): LongDelegate = LongDelegate()\n\
                  \n\
                  class Holder {\n\
                  \x20   operator fun MemberFactory.provideDelegate(owner: Holder, property: Any?): IntDelegate = IntDelegate()\n\
                  \x20   operator fun ExtensionFactory.provideDelegate(owner: Holder, property: Any?): IntDelegate = IntDelegate()\n\
                  \x20   val memberFirst by MemberFactory()\n\
                  \x20   val memberExtensionFirst by ExtensionFactory()\n\
                  }\n\
                  \n\
                  fun memberUse(): String = Holder().memberFirst\n\
                  fun memberExtensionUse(): Int = Holder().memberExtensionFirst\n\
                  \n\
                  fun unrelated() {\n\
                  \x20   val mismatched: Int = \"not an Int\"\n\
                  }\n";
    let expected = vec![
        "24:25: error: initializer type mismatch: expected 'Int', actual 'String'.".to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "DelegateProvideTower.kt",
            &dir.join("DelegateProvideTower-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// A member extension that reaches the delegate receiver but rejects the generated accessor
/// arguments remains diagnostic inventory; failure during instantiation must not erase it.
#[test]
fn an_inapplicable_member_extension_is_retained_in_the_delegate_diagnostic() {
    let source = "class Cell\n\
                  \n\
                  class Holder {\n\
                  \x20   operator fun Cell.getValue(owner: String, property: Any?): Long = 1\n\
                  \x20   val inferred by Cell()\n\
                  }\n\
                  \n\
                  fun unrelated() {\n\
                  \x20   val mismatched: Int = \"not an Int\"\n\
                  }\n";
    let expected = vec![
        "5:18: error: property delegate must have a 'getValue(Holder, KProperty1<Holder, Long>)' method. None of the following functions is applicable:\nfun Cell.getValue(owner: String, property: Any?): Long"
            .to_string(),
        "9:25: error: initializer type mismatch: expected 'Int', actual 'String'.".to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "InapplicableMemberExtensionDelegate.kt",
            &dir.join("InapplicableMemberExtensionDelegate-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// Selection may inspect lower rungs after an inapplicable member, but a total failure diagnoses
/// only the earliest non-empty candidate family. The sole member result therefore also determines
/// the property-reference result argument instead of becoming a star projection.
#[test]
fn inferred_delegate_failure_reports_the_earliest_candidate_family() {
    let source = "class Cell {\n\
                  \x20   operator fun getValue(owner: String, property: Any?): Long = 1\n\
                  }\n\
                  \n\
                  operator fun Cell.getValue(owner: Boolean, property: Any?): Long = 1\n\
                  \n\
                  class Holder {\n\
                  \x20   operator fun Cell.getValue(owner: Int, property: Any?): Long = 1\n\
                  \x20   val inferred by Cell()\n\
                  }\n";
    let expected = vec![
        "9:18: error: property delegate must have a 'getValue(Holder, KProperty1<Holder, Long>)' method. None of the following functions is applicable:\nfun getValue(owner: String, property: Any?): Long"
            .to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "InferredMixedDelegateCandidates.kt",
            &dir.join("InferredMixedDelegateCandidates-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// The checked-body selector owns the same earliest-family contract for a property whose explicit
/// type bypasses signature inference.
#[test]
fn explicit_delegate_failure_reports_the_earliest_candidate_family() {
    let source = "class Cell {\n\
                  \x20   operator fun getValue(owner: String, property: Any?): Long = 1\n\
                  }\n\
                  \n\
                  operator fun Cell.getValue(owner: Boolean, property: Any?): Long = 1\n\
                  \n\
                  class Holder {\n\
                  \x20   operator fun Cell.getValue(owner: Int, property: Any?): Long = 1\n\
                  \x20   val explicit: Long by Cell()\n\
                  }\n";
    let expected = vec![
        "9:24: error: property delegate must have a 'getValue(Holder, KProperty1<Holder, Long>)' method. None of the following functions is applicable:\nfun getValue(owner: String, property: Any?): Long"
            .to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "ExplicitMixedDelegateCandidates.kt",
            &dir.join("ExplicitMixedDelegateCandidates-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// Ordinary extension candidates render their extension receiver exactly once, before the callable
/// name. Their value-parameter list must not repeat the physical receiver slot.
#[test]
fn ordinary_extension_delegate_diagnostics_use_receiver_free_value_parameters() {
    let source = "class InapplicableCell\n\
                  operator fun InapplicableCell.getValue(owner: String, property: Any?): Long = 1\n\
                  val inapplicable by InapplicableCell()\n\
                  \n\
                  class AmbiguousCell\n\
                  operator fun AmbiguousCell.getValue(owner: String?, property: Any?): Int = 1\n\
                  operator fun AmbiguousCell.getValue(owner: Int?, property: Any?): Int = 2\n\
                  val ambiguous by AmbiguousCell()\n";
    let expected = vec![
        "3:18: error: property delegate must have a 'getValue(Nothing?, KProperty0<Long>)' method. None of the following functions is applicable:\nfun InapplicableCell.getValue(owner: String, property: Any?): Long"
            .to_string(),
        "8:15: error: overload resolution ambiguity on method 'getValue(Nothing?, KProperty0<*>)':\nfun AmbiguousCell.getValue(owner: String?, property: Any?): Int\nfun AmbiguousCell.getValue(owner: Int?, property: Any?): Int"
            .to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "OrdinaryExtensionDelegateDiagnostics.kt",
            &dir.join("OrdinaryExtensionDelegateDiagnostics-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// Anonymous classifier identity is backend/runtime state, never diagnostic source spelling.
#[test]
fn an_anonymous_owner_uses_the_source_anonymous_type_spelling_in_delegate_diagnostics() {
    let source = "class DispatchProbe {\n\
                  \x20   operator fun getValue(owner: Nothing?, property: Any?): Int = 1\n\
                  }\n\
                  \n\
                  fun inferred() = (object {\n\
                  \x20   val member by DispatchProbe()\n\
                  }).member\n";
    let expected = vec![
        "6:16: error: property delegate must have a 'getValue(<anonymous>, KProperty1<*, Int>)' method. None of the following functions is applicable:\nfun getValue(owner: Nothing?, property: Any?): Int"
            .to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "AnonymousDelegateOwner.kt",
            &dir.join("AnonymousDelegateOwner-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// An explicit property never enters delegate signature inference, so the same genuine ambiguity
/// is owned by the ordinary Pass-2 selector and must use the complete candidate diagnostic.
#[test]
fn an_explicit_property_reports_ambiguous_get_value() {
    let source = "class ExplicitFactory {\n\
                  \x20   operator fun getValue(owner: String?, property: Any?): Int = 1\n\
                  \x20   operator fun getValue(owner: Int?, property: Any?): Int = 2\n\
                  }\n\
                  \n\
                  val explicit: Int by ExplicitFactory()\n";
    let expected = vec![
        "6:19: error: overload resolution ambiguity on method 'getValue(Nothing?, KProperty0<*>)':\nfun getValue(owner: String?, property: Any?): Int\nfun getValue(owner: Int?, property: Any?): Int"
            .to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "ExplicitAmbiguousGetValue.kt",
            &dir.join("ExplicitAmbiguousGetValue-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// Rendering the one inapplicable candidate needs its inferred return type. The diagnostic path
/// demands that declaration once and propagates its result; it never retries another origin.
#[test]
fn an_inferred_candidate_result_is_demanded_for_the_delegate_ledger() {
    let source = "class DeferredResult {\n\
                  \x20   operator fun getValue(owner: Any, property: Any?) = 1\n\
                  }\n\
                  \n\
                  val inferred by DeferredResult()\n";
    let expected = vec![
        "5:14: error: property delegate must have a 'getValue(Nothing?, KProperty0<Int>)' method. None of the following functions is applicable:\nfun getValue(owner: Any, property: Any?): Int"
            .to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "DemandedDelegateResult.kt",
            &dir.join("DemandedDelegateResult-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}

/// A local classifier's self type includes the type parameters and values captured from its
/// enclosing inferred function. The delegate site carries that evaluated self type directly.
#[test]
fn a_capturing_local_class_delegate_keeps_its_applied_self_type() {
    let source = "class DispatchProbe<Owner> {\n\
                  \x20   operator fun getValue(owner: Owner, property: Any?): Int = 1\n\
                  }\n\
                  \n\
                  fun <Captured> inferred(value: Captured) = ({\n\
                  \x20   class LocalOwner<Own>(val own: Own) {\n\
                  \x20       val retained: Captured = value\n\
                  \x20       val member by DispatchProbe<LocalOwner<Own>>()\n\
                  \x20   }\n\
                  \x20   LocalOwner(value).member\n\
                  })()\n\
                  \n\
                  fun unrelated() {\n\
                  \x20   val mismatched: Int = \"not an Int\"\n\
                  }\n";
    let expected = vec![
        "14:25: error: initializer type mismatch: expected 'Int', actual 'String'.".to_string(),
    ];
    let dir = common::scratch_dir().expect("scratch dir");
    assert_eq!(
        kotlinc_error_ledger(
            source,
            "CapturedLocalDelegate.kt",
            &dir.join("CapturedLocalDelegate-ref"),
        ),
        expected,
        "kotlinc's complete ordered ledger",
    );
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected,
        "krusty's complete ordered ledger",
    );
}
