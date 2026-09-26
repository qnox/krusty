//! Resolver-local compatibility entry points for unit tests that construct legacy symbol tables.

use super::*;

pub(super) fn check_file(file: &File, syms: &mut SymbolTable, diags: &mut DiagSink) -> TypeInfo {
    check_file_at(file, diags.current_file(), syms, diags)
}

pub(super) fn check_file_at(
    file: &File,
    file_index: u32,
    syms: &mut SymbolTable,
    diags: &mut DiagSink,
) -> TypeInfo {
    check_file_on_checker_stack(file, file_index, None, syms, diags)
}

pub(super) fn check_file_in_source_set(
    files: &[File],
    file_index: u32,
    syms: &mut SymbolTable,
    diags: &mut DiagSink,
) -> TypeInfo {
    let file = &files[file_index as usize];
    check_file_on_checker_stack(file, file_index, Some(files), syms, diags)
}

/// Enter the check on a same-thread grown stack segment; `expr_with_context` rechecks the remaining
/// stack per recursion level so paths with large helper frames can chain further segments before
/// reaching [`crate::wide_stack::MAX_SEMANTIC_EXPR_DEPTH`]. This keeps the explicit depth guard —
/// not the calling thread's stack — authoritative without moving non-`Send` symbols or
/// caller-defined platform state (see [`crate::wide_stack`]).
fn check_file_on_checker_stack(
    file: &File,
    file_index: u32,
    source_files: Option<&[File]>,
    syms: &mut SymbolTable,
    diags: &mut DiagSink,
) -> TypeInfo {
    crate::wide_stack::on_wide_stack(move || {
        let published = syms.pass_one_symbols().is_some_and(|symbols| {
            file.anonymous_object_classes.values().all(|declaration| {
                symbols
                    .anonymous_object_capture_discovered
                    .contains(&(file_index, *declaration))
            })
        });
        let info = check_file_at_impl_mode(
            file,
            file_index,
            source_files,
            syms,
            None,
            diags,
            if published {
                CaptureDiscovery::Published
            } else {
                CaptureDiscovery::AtConstruction
            },
            None,
            None,
            None,
            None,
            None,
            SourceFragmentMode::Complete,
            None,
        );
        if !published {
            // Every object constructed in the file was reached, so one without captures captures
            // nothing.
            let mut discovered = info.anonymous_object_captures_by_class.clone();
            for declaration in file.anonymous_object_classes.values() {
                discovered.entry(*declaration).or_default();
            }
            syms.begin_module_mutation();
            install_anonymous_object_captures(syms, file_index, discovered);
            syms.finish_module_mutation();
        }
        info
    })
}
