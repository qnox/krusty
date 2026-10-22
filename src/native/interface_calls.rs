//! How a call through a module interface meets its implementations: the contract between the code
//! generator, which emits the calls and the types, and the linker, which alone sees every type of
//! the program.
//!
//! A call through an interface member names the member's module-wide number, so it cannot know
//! which classes implement it: a class of any file may. Each call therefore goes to a per-member
//! stub, `kt_itf_<module>_<number>`, that no object defines. Every object instead records, for
//! each type it emits that has instances, which function stands at each number of that type's
//! interface table. The final link reads those records and writes each stub: a direct jump when
//! one function implements the member in the whole program, one type comparison and two direct
//! jumps when there are two, and otherwise the interface table lookup every call used to make.
//!
//! A record is three pointer-sized words: the member number, then the type descriptor and the
//! function at that number of the type's table, each filled by a relocation. An object's records
//! live in a section of their own named after the object's module, which the linker reads and
//! does not load. A record names no stub: the call sites alone decide which stubs exist, and a
//! record only describes what an implementation's own signature is, which need not be the
//! member's.

use super::symbols::module_namespace;

/// The prefix of the section an object's implementation records are emitted in; the module's
/// namespace follows it (see [`implementations_section`]).
pub(super) const IMPLEMENTATIONS_SECTION: &str = ".kt_interface_implementations";

/// The size of one record: the member number, the type and the implementation, a word each.
pub(super) const RECORD_SIZE: u64 = 24;

/// Where a `KType` pointer sits in an object, and where the interface table pointer sits in a
/// `KType` (`krusty_rt.h`): the two loads a table lookup makes.
pub(super) const RECEIVER_TYPE_OFFSET: u32 = 0;
pub(super) const ITABLE_OFFSET: u32 = 152;

const STUB_PREFIX: &str = "kt_itf_";

/// The stub a call through member `number` of `module_name`'s interfaces goes to.
pub(super) fn stub_symbol(module_name: &str, number: u32) -> String {
    stub_in_namespace(&module_namespace(module_name), number)
}

/// The section `module_name`'s implementation records are emitted in.
pub(super) fn implementations_section(module_name: &str) -> String {
    format!(
        "{IMPLEMENTATIONS_SECTION}.{}",
        module_namespace(module_name)
    )
}

/// The stub a record of the section `section` names with `number`, or `None` for a section that
/// holds no implementation records.
pub(super) fn recorded_stub(section: &str, number: u32) -> Option<String> {
    let namespace = section
        .strip_prefix(IMPLEMENTATIONS_SECTION)?
        .strip_prefix('.')?;
    (!namespace.is_empty()).then(|| stub_in_namespace(namespace, number))
}

/// Whether `section` holds implementation records, which the link reads and does not load.
pub(super) fn is_implementations_section(section: &str) -> bool {
    recorded_stub(section, 0).is_some()
}

fn stub_in_namespace(namespace: &str, number: u32) -> String {
    format!("{STUB_PREFIX}{namespace}_{number}")
}

/// The member number a stub symbol names, or `None` for any other symbol.
pub(super) fn stub_number(symbol: &str) -> Option<u32> {
    let rest = symbol.strip_prefix(STUB_PREFIX)?;
    let (namespace, number) = rest.rsplit_once('_')?;
    if namespace.is_empty() || number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    number.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stub_symbol_names_its_member_number() {
        let symbol = stub_symbol("my-module_2", 17);
        assert_eq!(symbol, "kt_itf_my_2d_module_5f_2_17");
        assert_eq!(stub_number(&symbol), Some(17));
    }

    #[test]
    fn a_record_section_names_the_stubs_of_its_module() {
        let section = implementations_section("my-module_2");
        assert_eq!(section, ".kt_interface_implementations.my_2d_module_5f_2");
        assert_eq!(
            recorded_stub(&section, 3),
            Some(stub_symbol("my-module_2", 3))
        );
        assert!(is_implementations_section(&section));
    }

    #[test]
    fn other_sections_hold_no_records() {
        for section in [
            ".kt_interface_implementations",
            ".kt_interface_implementations.",
            ".kt_interface_implementationsbox",
            ".rodata",
        ] {
            assert_eq!(recorded_stub(section, 0), None, "{section}");
            assert!(!is_implementations_section(section), "{section}");
        }
    }

    #[test]
    fn other_symbols_name_no_member_number() {
        assert_eq!(stub_number("kt_mod_box_3"), None);
        assert_eq!(stub_number("kt_itf_box"), None);
        assert_eq!(stub_number("kt_itf__3"), None);
        assert_eq!(stub_number("kt_itf_box_x3"), None);
    }
}
