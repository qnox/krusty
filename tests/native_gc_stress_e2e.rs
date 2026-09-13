//! The collector under load, through the code generator that lays its objects out.
//!
//! `native_gc_e2e.rs` tests the collector from C against types written by hand. These tests are the
//! other half: the same collector against types the GENERATOR emitted — its field offsets, its
//! reference tables, its closure captures, its array strides — driven by Kotlin programs that
//! allocate far past the collection threshold while holding a live set that is verified afterwards.
//! A wrong offset frees a live object or follows an integer as a pointer, and the only way to see
//! that is to keep something alive across many collections and then read it.
//!
//! Every program here is deterministic: the churn is a fixed number of iterations and the answer
//! is exact. A collector bug shows up as damaged data, a crash, or a number that is not the one
//! written down — never as flakiness.
//!
//! The collection threshold is bytes allocated since the last collection, so a loop that builds
//! and drops strings is what forces collections; the counts below are far past it.
//!
//! Skips (never fails) when this build of krusty carries no prebuilt runtime for the host.

use std::path::{Path, PathBuf};

use krusty::backend::Artifact;
use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::native::{CraneliftBackend, NativeTarget};
use krusty::source::SourceInput;

use super::common;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "krusty-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn host() -> Option<NativeTarget> {
    let target = NativeTarget::host()?;
    (krusty::native::can_link(target) && krusty::toolchain::stdlib_jar().is_some())
        .then_some(target)
}

/// Compile one program with the native code generator for the host.
fn compile(source: &str) -> (Vec<Artifact>, Vec<String>) {
    let target = host().expect("checked by the caller");
    let jar = krusty::toolchain::stdlib_jar().expect("checked by the caller");
    let classpath = std::rc::Rc::new(Classpath::new(vec![jar]));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(
        classpath.clone(),
    ));
    let inputs = vec![SourceInput::kotlin(source).with_file_stem("Main")];
    let mut diags = DiagSink::new();
    let mut features = krusty::features::LangFeatures::new();
    features.apply_source_directives(source);
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, &features, &mut diags,
    );
    let backend = CraneliftBackend::new(classpath, target);
    let artifacts = krusty::compiler::emit_analyzed(
        analysis,
        &["Main".to_string()],
        &backend,
        "main",
        &mut diags,
    );
    (artifacts, diags.diags.into_iter().map(|d| d.msg).collect())
}

/// Compile, link and run; return standard output.
fn run(source: &str) -> String {
    let target = host().expect("checked by the caller");
    let (artifacts, diagnostics) = compile(source);
    assert!(
        diagnostics.is_empty(),
        "the program must compile: {diagnostics:?}"
    );
    let objects = artifacts
        .iter()
        .map(|(_, bytes)| bytes.as_slice())
        .collect::<Vec<_>>();
    let image = krusty::native::link_program(&objects, target).expect("link");
    let scratch = Scratch::new("stress");
    let executable = scratch.path().join("program");
    std::fs::write(&executable, &image).expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = common::run_freshly_written(std::process::Command::new(&executable).env_clear())
        .expect("run the built executable");
    assert!(
        output.status.success(),
        "the program must exit cleanly: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A loop that allocates and drops strings: several heap objects per iteration, all garbage by the
/// next. Appended to a program to force many collections while its live set is held.
const CHURN: &str = "fun churn(rounds: Int): String {\n\
                     \x20   var last = \"\"\n\
                     \x20   var i = 0\n\
                     \x20   while (i < rounds) { last = \"garbage-$i-${i * 2}\"; i = i + 1 }\n\
                     \x20   return last\n\
                     }\n";

#[test]
fn a_live_object_graph_survives_repeated_collection_intact() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A chain of 20,000 nodes, each holding a heap string, rooted only by its head — and churn
    // between every batch, so the chain is traced many times while it is still being built. The
    // walk afterwards checks every node: a missed reference field frees the tail, and a wrong
    // offset reads a neighbour's bytes.
    assert_eq!(
        run(&format!(
            "{CHURN}\
             class Node(val value: Int, val label: String, val next: Node?)\n\
             fun main() {{\n\
             \x20   var head: Node? = null\n\
             \x20   var i = 0\n\
             \x20   while (i < 20000) {{\n\
             \x20       head = Node(i, \"node-$i\", head)\n\
             \x20       if (i % 2000 == 0) churn(4000)\n\
             \x20       i = i + 1\n\
             \x20   }}\n\
             \x20   churn(40000)\n\
             \x20   var count = 0\n\
             \x20   var sum = 0\n\
             \x20   var labels = 0\n\
             \x20   var n = head\n\
             \x20   while (n != null) {{\n\
             \x20       sum = sum + n.value\n\
             \x20       if (n.label == \"node-${{n.value}}\") labels = labels + 1\n\
             \x20       count = count + 1\n\
             \x20       n = n.next\n\
             \x20   }}\n\
             \x20   println(count)\n\
             \x20   println(sum)\n\
             \x20   println(labels)\n\
             }}\n"
        )),
        "20000\n199990000\n20000\n",
        "every node, its integer and its string must come through every collection intact"
    );
}

#[test]
fn a_replaced_array_entry_becomes_garbage_and_the_rest_stays() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A fixed-size array used as a cache: each round overwrites one slot, so the object that was
    // there becomes unreachable while the other 63 stay live. Mark-sweep needs no write barrier
    // for this, which is exactly the claim being tested — the new value is found by tracing, and
    // the old one is not retained by having once been there.
    assert_eq!(
        run(&format!(
            "{CHURN}\
             class Entry(val slot: Int, val round: Int, val text: String)\n\
             fun main() {{\n\
             \x20   val cache = arrayOfNulls<Entry>(64)\n\
             \x20   var round = 0\n\
             \x20   while (round < 2000) {{\n\
             \x20       val slot = round % 64\n\
             \x20       cache[slot] = Entry(slot, round, \"entry-$slot-$round\")\n\
             \x20       if (round % 200 == 0) churn(8000)\n\
             \x20       round = round + 1\n\
             \x20   }}\n\
             \x20   churn(40000)\n\
             \x20   var intact = 0\n\
             \x20   var i = 0\n\
             \x20   while (i < 64) {{\n\
             \x20       val e = cache[i]\n\
             \x20       if (e != null && e.slot == i && e.text == \"entry-$i-${{e.round}}\") {{\n\
             \x20           intact = intact + 1\n\
             \x20       }}\n\
             \x20       i = i + 1\n\
             \x20   }}\n\
             \x20   println(intact)\n\
             \x20   println(cache[0]!!.round)\n\
             \x20   println(cache[63]!!.round)\n\
             }}\n"
        )),
        "64\n1984\n1983\n",
        "the last writer of each slot is live and intact; the ones it replaced are not retained"
    );
}

#[test]
fn references_held_only_in_deep_frames_survive_collection() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // 400 nested frames, each holding a heap string that nothing else references, with collections
    // triggered at the bottom. Roots are found conservatively by scanning the stack and the
    // callee-saved registers, so this is the property that scan exists for: a reference alive only
    // in a frame the program has not returned from yet must be found wherever the register
    // allocator put it.
    assert_eq!(
        run(&format!(
            "{CHURN}\
             fun descend(depth: Int): String {{\n\
             \x20   val mine = \"frame-$depth\"\n\
             \x20   if (depth == 0) {{\n\
             \x20       churn(40000)\n\
             \x20       return mine\n\
             \x20   }}\n\
             \x20   val below = descend(depth - 1)\n\
             \x20   if (below == \"\") return \"\"\n\
             \x20   return if (mine == \"frame-$depth\") mine else \"\"\n\
             }}\n\
             fun main() {{ println(descend(400)) }}\n"
        )),
        "frame-400\n",
        "a string held only by a frame that has not returned must survive the collection under it"
    );
}

#[test]
fn closures_and_their_captures_survive_collection() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // Function values stored in an array, each capturing a heap string and a shared mutable cell.
    // The closures are the only reference to their captures, and the array the only reference to
    // the closures — so tracing has to follow array element to function object to capture, using
    // the offsets the generator laid the captures out at.
    assert_eq!(
        run(&format!(
            "{CHURN}\
             fun main() {{\n\
             \x20   var calls = 0\n\
             \x20   val fns = arrayOfNulls<() -> String>(32)\n\
             \x20   var i = 0\n\
             \x20   while (i < 32) {{\n\
             \x20       val mine = \"capture-$i\"\n\
             \x20       fns[i] = {{ calls = calls + 1; mine }}\n\
             \x20       i = i + 1\n\
             \x20   }}\n\
             \x20   churn(60000)\n\
             \x20   var intact = 0\n\
             \x20   i = 0\n\
             \x20   while (i < 32) {{\n\
             \x20       if (fns[i]!!() == \"capture-$i\") intact = intact + 1\n\
             \x20       i = i + 1\n\
             \x20   }}\n\
             \x20   println(intact)\n\
             \x20   println(calls)\n\
             }}\n"
        )),
        "32\n32\n",
        "each closure keeps its own capture, and the shared cell is one cell"
    );
}

#[test]
fn every_kind_of_root_holds_its_object_at_once() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // A top-level property, an `object` singleton's field, a class instance's field, an array
    // element, a closure capture and a local — every place the generator can put a reference,
    // live simultaneously across heavy churn. Each is registered or traced by a different
    // mechanism, and a collector that is right about each alone can still be wrong about them
    // together.
    assert_eq!(
        run(&format!(
            "{CHURN}\
             var topLevel: String = \"\"\n\
             object Registry {{ var held: String = \"\" }}\n\
             class Holder(var field: String)\n\
             fun main() {{\n\
             \x20   topLevel = \"top-${{1 + 1}}\"\n\
             \x20   Registry.held = \"registry-${{2 + 1}}\"\n\
             \x20   val holder = Holder(\"holder-${{3 + 1}}\")\n\
             \x20   val slots = arrayOfNulls<String>(2)\n\
             \x20   slots[0] = \"slot-${{4 + 1}}\"\n\
             \x20   slots[1] = \"slot-${{5 + 1}}\"\n\
             \x20   val captured = \"captured-${{6 + 1}}\"\n\
             \x20   val closure = {{ captured }}\n\
             \x20   val local = \"local-${{7 + 1}}\"\n\
             \x20   churn(80000)\n\
             \x20   println(topLevel)\n\
             \x20   println(Registry.held)\n\
             \x20   println(holder.field)\n\
             \x20   println(slots[0])\n\
             \x20   println(slots[1])\n\
             \x20   println(closure())\n\
             \x20   println(local)\n\
             }}\n"
        )),
        "top-2\nregistry-3\nholder-4\nslot-5\nslot-6\ncaptured-7\nlocal-8\n",
        "every root mechanism must hold its object through the same collections"
    );
}

#[test]
fn interleaved_object_sizes_reuse_their_slots() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // The heap segregates objects by size class, so a program that alternates between small
    // objects and large arrays exercises several classes at once — and one large allocation per
    // round, which takes the large-object path and returns its mapping when reclaimed. The memory
    // this program needs does not grow with the number of rounds, which is the claim; if slots
    // were not reused it would run out.
    assert_eq!(
        run(&format!(
            "{CHURN}\
             class Small(val a: Int, val b: String)\n\
             fun main() {{\n\
             \x20   var kept: Small? = null\n\
             \x20   var round = 0\n\
             \x20   while (round < 400) {{\n\
             \x20       val big = IntArray(20000)\n\
             \x20       big[0] = round\n\
             \x20       big[19999] = round * 2\n\
             \x20       if (big[0] + big[19999] != round * 3) {{ println(\"damaged\"); return }}\n\
             \x20       var i = 0\n\
             \x20       while (i < 200) {{ Small(i, \"small-$i\"); i = i + 1 }}\n\
             \x20       if (round == 399) kept = Small(round, \"kept-$round\")\n\
             \x20       round = round + 1\n\
             \x20   }}\n\
             \x20   churn(20000)\n\
             \x20   println(kept!!.a)\n\
             \x20   println(kept!!.b)\n\
             }}\n"
        )),
        "399\nkept-399\n",
        "a program that allocates far more than it keeps must run in the memory it keeps"
    );
}
