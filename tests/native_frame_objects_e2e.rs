//! Objects a function creates and only reads the fields of, which Native keeps in the function's
//! frame instead of the heap. Each program is checked against the JVM first, so the answer is
//! Kotlin's, and then on Native, where the frame placement must not change it.

use object::{Object, ObjectSection, ObjectSymbol, RelocationTarget};

use krusty::backend::Backend as _;
use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::native::{CraneliftBackend, Entry, NativeTarget};
use krusty::source::SourceInput;

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

fn expect_ok_everywhere(source: &str, stem: &str) {
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// Compile one source far enough to inspect the relocatable object. This does not need the host C
/// linker or runtime: the allocation decision is already final in the Cranelift object.
fn native_object(source: &str, stem: &str) -> Option<Vec<u8>> {
    let target = NativeTarget::host()?;
    let jar = krusty::toolchain::stdlib_jar().expect("the native tests require the stdlib jar");
    let classpath = std::rc::Rc::new(Classpath::new(vec![jar]));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    let inputs = [SourceInput::kotlin(source).with_file_stem(stem)];
    let features = krusty::features::LangFeatures::new();
    let mut diags = DiagSink::new();
    let backend = CraneliftBackend::new(target).with_entry(Entry::Box);
    let platform = krusty::frontend::PlatformProvider::new(backend.compilation_target(), platform);
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, &features, &mut diags,
    );
    let artifacts =
        krusty::compiler::emit_analyzed(analysis, &[stem.to_string()], &backend, stem, &mut diags);
    let messages = diags
        .diags
        .into_iter()
        .map(|diagnostic| diagnostic.msg)
        .collect::<Vec<_>>();
    assert_eq!(messages, Vec::<String>::new(), "{stem}");
    let mut objects = artifacts
        .into_iter()
        .filter(|(name, _)| name.ends_with(".o"))
        .map(|(_, bytes)| bytes)
        .collect::<Vec<_>>();
    assert_eq!(objects.len(), 1, "{stem}: expected one emitted object");
    objects.pop()
}

fn allocation_relocations(object: &[u8]) -> usize {
    let file = object::File::parse(object).expect("the native object parses");
    let mut count = 0;
    for section in file.sections() {
        for (_, relocation) in section.relocations() {
            let RelocationTarget::Symbol(index) = relocation.target() else {
                continue;
            };
            if file
                .symbol_by_index(index)
                .and_then(|symbol| symbol.name())
                .is_ok_and(|name| name == "kt_gc_allocate")
            {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn frame_placement_removes_only_the_non_escaping_allocation() {
    let Some(frame) = native_object(
        "class Point(val x: Int, val y: Int)\n\
         fun value(): Int { val point = Point(1, 2); return point.x + point.y }\n\
         fun box() = if (value() == 3) \"OK\" else \"fail\"\n",
        "FrameAllocation",
    ) else {
        return;
    };
    assert_eq!(allocation_relocations(&frame), 0);

    let heap = native_object(
        "class Point(val x: Int, val y: Int)\n\
         fun made(): Point { val point = Point(1, 2); return point }\n\
         fun box() = if (made().x + made().y == 3) \"OK\" else \"fail\"\n",
        "HeapAllocation",
    )
    .expect("the host target was available for the frame control");
    assert_eq!(allocation_relocations(&heap), 1);
}

#[test]
fn a_local_object_read_for_its_fields_keeps_them_in_every_iteration() {
    // A fresh object per iteration, read only through its properties: each iteration's object is
    // its own, and the last one's values are the ones read.
    expect_ok_everywhere(
        "class Point(val x: Int, val y: Int)\n\
         fun box(): String {\n\
         \x20   var sum = 0L\n\
         \x20   for (i in 0 until 100000) {\n\
         \x20       val p = Point(i, i + 1)\n\
         \x20       sum += (p.x xor p.y).toLong()\n\
         \x20   }\n\
         \x20   return if (sum == 1656000L) \"OK\" else \"fail: $sum\"\n\
         }\n",
        "FramePoints",
    );
}

#[test]
fn a_frame_object_keeps_what_its_fields_reference_through_a_collection() {
    // The object's field holds the only reference to a heap string while the loop allocates
    // enough garbage to collect several times. The collector finds that reference in the frame.
    expect_ok_everywhere(
        "class Holder(val text: String, val n: Int)\n\
         fun churn(rounds: Int): Int {\n\
         \x20   var length = 0\n\
         \x20   for (i in 0 until rounds) length += (\"garbage-\" + i).length\n\
         \x20   return length\n\
         }\n\
         fun box(): String {\n\
         \x20   for (i in 0 until 20) {\n\
         \x20       val holder = Holder(\"held-\" + i, i)\n\
         \x20       churn(50000)\n\
         \x20       if (holder.text != \"held-\" + i || holder.n != i) return \"fail at $i: ${holder.text}\"\n\
         \x20   }\n\
         \x20   return \"OK\"\n\
         }\n",
        "FrameHolder",
    );
}

#[test]
fn an_object_that_leaves_its_function_stays_on_the_heap() {
    // Every way out: returned, stored in a collection, captured by a lambda, passed to a call,
    // compared by identity, reassigned. None of these may be a frame object, and each must still
    // read correctly after the function that made it has returned.
    expect_ok_everywhere(
        "class Box(val value: Int)\n\
         fun made(i: Int): Box { val b = Box(i); return b }\n\
         fun valueOf(b: Box) = b.value\n\
         fun box(): String {\n\
         \x20   val kept = ArrayList<Box>()\n\
         \x20   for (i in 0 until 3) { val b = Box(i); kept.add(b) }\n\
         \x20   val c = Box(10)\n\
         \x20   val read = { c.value }\n\
         \x20   val d = Box(20)\n\
         \x20   val e = Box(30)\n\
         \x20   val same = e === e\n\
         \x20   var f = Box(40)\n\
         \x20   f = Box(f.value + 1)\n\
         \x20   val total = made(5).value + kept[0].value + kept[1].value + kept[2].value + read() + valueOf(d)\n\
         \x20   if (total != 38 || !same || f.value != 41) return \"fail: $total $same ${f.value}\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "HeapBoxes",
    );
}

#[test]
fn a_class_whose_constructor_does_more_than_store_stays_on_the_heap() {
    // An `init` block may let `this` out, and an open class or one with a getter may be read
    // through code that keeps it. The program reads the object only through its fields, so only
    // the class decides.
    expect_ok_everywhere(
        "val seen = ArrayList<Any>()\n\
         class Registered(val n: Int) { init { seen.add(this) } }\n\
         open class Base(val n: Int)\n\
         class Computed(val n: Int) { val twice: Int get() = n * 2 }\n\
         fun box(): String {\n\
         \x20   val r = Registered(1)\n\
         \x20   val b = Base(2)\n\
         \x20   val c = Computed(3)\n\
         \x20   if (r.n + b.n + c.twice != 9) return \"fail sum\"\n\
         \x20   if (seen.size != 1 || (seen[0] as Registered).n != 1) return \"fail seen\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "HeapClasses",
    );
}

#[test]
fn a_frame_object_read_inside_a_lambda_of_its_function_keeps_its_fields() {
    // The lambda runs while the function that made the object is still running, and reads the
    // object only for a field. Whether or not the object lives in the frame, the lambda must see
    // the values the construction gave it.
    expect_ok_everywhere(
        "class Pair2(val a: Int, val b: String)\n\
         fun box(): String {\n\
         \x20   val p = Pair2(4, \"four\")\n\
         \x20   val read = { p.b + p.a }\n\
         \x20   val direct = p.a\n\
         \x20   return if (read() == \"four4\" && direct == 4) \"OK\" else \"fail: ${read()}\"\n\
         }\n",
        "FrameLambda",
    );
}

#[test]
fn a_frame_object_converts_and_guards_its_fields_as_the_heap_object_does() {
    // Fields of every carrier the constructor converts into, a nullable one left null, a
    // `lateinit` one no constructor assigns (read before assignment raises Kotlin's exception),
    // and a class whose companion the construction creates first.
    expect_ok_everywhere(
        "var created = 0\n\
         class Mixed(val d: Double, val f: Float, val l: Long, val c: Char, val b: Boolean, val n: String?) {\n\
         \x20   lateinit var later: String\n\
         \x20   companion object { init { created++ } }\n\
         }\n\
         fun box(): String {\n\
         \x20   var total = 0.0\n\
         \x20   for (i in 0 until 3) {\n\
         \x20       val m = Mixed(i + 0.5, i * 2f, i * 10L, 'a' + i, i % 2 == 0, null)\n\
         \x20       total += m.d + m.f + m.l + m.c.code + (if (m.b) 1 else 0) + (m.n?.length ?: 0)\n\
         \x20   }\n\
         \x20   if (total != 336.5 || created != 1) return \"fail: $total $created\"\n\
         \x20   val m = Mixed(0.0, 0f, 0L, 'x', false, \"set\")\n\
         \x20   try {\n\
         \x20       m.later.length\n\
         \x20       return \"fail: read an unassigned lateinit\"\n\
         \x20   } catch (e: UninitializedPropertyAccessException) {\n\
         \x20       if (e.message != \"lateinit property later has not been initialized\") return \"fail: ${e.message}\"\n\
         \x20   }\n\
         \x20   return if (m.n == \"set\") \"OK\" else \"fail n\"\n\
         }\n",
        "FrameMixed",
    );
}
