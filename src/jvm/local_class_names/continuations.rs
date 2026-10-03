//! The names of suspend functions' continuation classes and the declaration paths kotlinc gives
//! them.

use crate::ir::{IrExpr, IrFile};
use crate::types::{type_name, TypeName};

/// How many SUSPEND functions sharing this one's continuation NAME the file declares before it.
///
/// A continuation class is named after the method it re-enters, so two overloads would share one —
/// and they do not share a spill layout, so whichever class loses the name resumes against fields it
/// does not have (`NoSuchFieldError`). Both machines number the later one.
///
/// This answers for a suspend function that has NO source declaration behind it — a lowering-made
/// one, which no frontend pass could have reserved a position for. A declared function reads its
/// position from [`continuation_ordinal`] instead of counting anything here.
fn same_name_ordinal(ir: &IrFile, fid: u32) -> usize {
    let function = &ir.functions[fid as usize];
    let name = continuation_source_name(ir, fid);
    ir.functions
        .iter()
        .enumerate()
        .take(fid as usize)
        .filter(|(other_fid, other)| {
            continuation_source_name(ir, *other_fid as u32) == name
                && other.dispatch_receiver == function.dispatch_receiver
                && ir.suspend_funs.contains(&(*other_fid as u32))
        })
        .count()
}

/// The source identity used in a generated continuation class name.
///
/// A source-declared function keeps this independently from its physical JVM method name, which
/// may be value-class-mangled or changed by `@JvmName`. A lowering-made function has no source
/// declaration and therefore owns its generated name directly. In particular, never split a JVM
/// name on `-`: that character is valid inside a backticked Kotlin identifier.
///
/// A lifted lambda or local function records its enclosing callable as its source name, which is
/// not its own identity: its continuation keeps the implementation name it was lifted under, so it
/// cannot take the `<owner>$<enclosing>$N` name of the enclosing function's own classes.
pub(crate) fn continuation_source_name(ir: &IrFile, fid: u32) -> &str {
    if ir.lambda_origins.contains_key(&fid) || ir.lifted_functions.contains_key(&fid) {
        return &ir.functions[fid as usize].name;
    }
    match ir.fn_source_names.get(&fid) {
        Some(name) => name,
        None => {
            assert!(
                !ir.fn_source_order.contains_key(&fid),
                "source suspend function {fid} has no recorded source name"
            );
            &ir.functions[fid as usize].name
        }
    }
}

/// The 1-based `$N` the continuation class of `fid` takes in its `<owner>$<function>` sequence.
///
/// The sequence is shared with the anonymous objects those bodies declare, and the pass that names
/// those objects is the one that leaves a position free for each suspend function, in declaration
/// order. It publishes which position it left — `IrFile::fn_continuation_ordinal` — so there is one
/// numbering, computed once. Nothing here re-derives it from a class name.
pub(crate) fn continuation_ordinal(ir: &IrFile, fid: u32) -> usize {
    match ir.fn_continuation_ordinal.get(&fid) {
        Some(&ordinal) => ordinal as usize,
        None => {
            assert!(
                !ir.fn_source_order.contains_key(&fid),
                "source suspend function {fid} has no published continuation ordinal"
            );
            // A lowering-made function has no source declaration, so no anonymous source object
            // can consume its generated sequence. Its target-private overloads number themselves.
            // A callable-reference SAM adapter is emitted as its own class at this same
            // `{owner}${function}$1` spelling. Leaving the continuation there replaces the
            // adapter, and the call site still constructs it with the function value.
            let occupied = usize::from(sam_adapter_class_occupies_first_ordinal(ir, fid));
            same_name_ordinal(ir, fid) + 1 + occupied
        }
    }
}

/// Whether `fid` is the forwarding method of a callable-reference SAM adapter.
///
/// That adapter is always a class and occupies ordinal 1 of the implementation function's
/// generated-class sequence. The exact `impl_fn` edge and checked `function_adapter` role identify
/// it; the function's generated spelling is not semantic input.
fn sam_adapter_class_occupies_first_ordinal(ir: &IrFile, fid: u32) -> bool {
    ir.exprs.iter().any(|expression| {
        matches!(
            expression,
            IrExpr::Lambda {
                impl_fn,
                sam: Some(target),
                ..
            } if *impl_fn == fid && target.function_adapter
        )
    })
}

/// The continuation class for an ordinal from [`continuation_ordinal`]. The one place a generated
/// continuation class is spelled.
pub(crate) fn continuation_class_name(owner: &str, function: &str, ordinal: usize) -> String {
    format!("{owner}${function}${ordinal}")
}

/// Name the continuation class of suspend function `fid` under `owner`, its dispatch receiver
/// `host`'s internal name or the file facade's, and record kotlinc's `fqNameWhenAvailable` of it:
/// kotlinc declares a continuation class in the function it drives (`Task.run.<no name provided>`).
///
/// The name uses the recorded SOURCE function name, never the value-class-mangled JVM name. A
/// backticked source identifier may itself contain `-`, so physical spelling cannot recover it.
pub(crate) fn name_continuation(
    ir: &mut IrFile,
    fid: u32,
    owner: &str,
    host: Option<TypeName>,
    facade: &str,
) -> String {
    let function = continuation_source_name(ir, fid);
    let name = continuation_class_name(owner, function, continuation_ordinal(ir, fid));
    let path = format!(
        "{}.{function}.<no name provided>",
        super::host_path(ir, host, facade)
    );
    ir.declaration_paths.insert(type_name(&name), path);
    name
}
