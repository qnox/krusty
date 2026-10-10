//! The libraries a program imports functions from, as the linker needs to know them.
//!
//! A program that calls C (through a cinterop library or `platform.*`) needs the C library and
//! whatever libraries its interop declarations name. They are linked the way Kotlin/Native and
//! every C toolchain link them: dynamically, so the executable names each library it needs and the
//! system's loader binds the calls when the program starts.
//!
//! What the linker needs from a library is small: the name the loader looks it up by, and which
//! functions it exports at which symbol version. [`ImportLibrary`] is exactly that and nothing
//! more, so a link never has to read the target's library files. The facts are captured once, when
//! a cinterop library is made against the target's headers and libraries, and travel with it; a
//! program built later, for any target from any host, needs no sysroot. Reading an ELF shared
//! object ([`ImportLibrary::from_elf_shared_object`]) is one way of capturing them.

use std::collections::HashMap;

use object::elf::{FileHeader64, DT_SONAME, ET_DYN, SHT_DYNSYM, STB_GLOBAL, STB_WEAK};
use object::read::elf::{FileHeader, SectionTable, Sym};
use object::{LittleEndian, SymbolIndex};

use super::super::target::Arch;
use super::ProgramLinkError;

/// What a symbol a library exports is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportKind {
    /// Code, including an indirect function the loader resolves to an implementation.
    Function,
    /// A variable or any other non-code symbol.
    Data,
}

/// One symbol a library defines for other objects to bind to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    pub kind: ExportKind,
    /// The default version a reference binds to (`name@@VERSION`), or `None` when the library
    /// does not version the symbol.
    pub version: Option<String>,
}

/// A library a program may import functions from.
///
/// Every name it holds is written into the image as a NUL-terminated string the loader matches
/// byte for byte, so construction ([`ImportLibrary::new`]) refuses a name that would not survive
/// that exactly: an empty one, or one containing NUL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportLibrary {
    soname: String,
    exports: HashMap<String, Export>,
}

impl ImportLibrary {
    /// A library the loader finds by `soname` (on Linux, its `DT_NEEDED` entry), exporting
    /// `exports`.
    pub fn new(
        soname: String,
        exports: impl IntoIterator<Item = (String, Export)>,
    ) -> Result<Self, ProgramLinkError> {
        loader_string("library name", &soname, &soname)?;
        let mut checked = HashMap::new();
        for (name, export) in exports {
            loader_string("exported name", &name, &soname)?;
            if let Some(version) = &export.version {
                loader_string(&format!("version of `{name}`"), version, &soname)?;
            }
            if checked.insert(name.clone(), export).is_some() {
                return Err(ProgramLinkError::InvalidLibrary(format!(
                    "{soname} exports `{name}` twice"
                )));
            }
        }
        Ok(Self {
            soname,
            exports: checked,
        })
    }

    /// The name the loader finds the library by.
    pub fn soname(&self) -> &str {
        &self.soname
    }

    pub fn export(&self, name: &str) -> Option<&Export> {
        self.exports.get(name)
    }

    /// Read an ELF shared object for `arch`; `what` names it in errors (usually its path).
    ///
    /// The loader name is the library's `DT_SONAME`. A library without one is found by whatever
    /// name it was linked as, which only the caller knows, so `needed` must give it then; when
    /// the library does have a `DT_SONAME`, `needed` may only repeat it.
    pub fn from_elf_shared_object(
        what: &str,
        bytes: &[u8],
        arch: Arch,
        needed: Option<&str>,
    ) -> Result<Self, ProgramLinkError> {
        read(what, bytes, arch, needed)
    }
}

/// Check that `text` reaches the loader exactly: not empty, and no NUL to end it early.
fn loader_string(role: &str, text: &str, library: &str) -> Result<(), ProgramLinkError> {
    if text.is_empty() {
        return Err(ProgramLinkError::InvalidLibrary(format!(
            "{role} is empty in library {library:?}"
        )));
    }
    if text.contains('\0') {
        return Err(ProgramLinkError::InvalidLibrary(format!(
            "{role} {text:?} in library {library:?} contains NUL"
        )));
    }
    Ok(())
}

fn read(
    what: &str,
    bytes: &[u8],
    arch: Arch,
    needed: Option<&str>,
) -> Result<ImportLibrary, ProgramLinkError> {
    let malformed =
        |error: object::Error| ProgramLinkError::Parse(format!("shared library {what}: {error}"));
    if bytes.len() < 64 || !bytes.starts_with(b"\x7fELF") {
        return Err(ProgramLinkError::Parse(format!(
            "shared library {what} is not an ELF file"
        )));
    }
    if bytes[4] != 2 || bytes[5] != 1 {
        return Err(ProgramLinkError::ForeignObject(format!(
            "shared library {what} is not a 64-bit little-endian ELF file; {arch:?} links only those"
        )));
    }
    let header = FileHeader64::<LittleEndian>::parse(bytes).map_err(malformed)?;
    let endian = header.endian().map_err(malformed)?;
    if header.e_type(endian) != ET_DYN {
        return Err(ProgramLinkError::Parse(format!(
            "shared library {what} is not a shared object (e_type {})",
            header.e_type(endian)
        )));
    }
    let machine = header.e_machine(endian).0;
    if machine != arch.elf_machine() {
        return Err(ProgramLinkError::ForeignObject(format!(
            "shared library {what} is code for ELF machine {machine}, not {arch:?} (machine {})",
            arch.elf_machine()
        )));
    }
    let sections = header.sections(endian, bytes).map_err(malformed)?;
    let soname = match (soname(&sections, bytes).map_err(malformed)?, needed) {
        (Some(soname), None) => exact(what, "DT_SONAME", soname)?,
        (Some(soname), Some(needed)) => {
            let soname = exact(what, "DT_SONAME", soname)?;
            if soname != needed {
                return Err(ProgramLinkError::InvalidLibrary(format!(
                    "shared library {what} is named {soname:?} by its DT_SONAME, not {needed:?}"
                )));
            }
            soname
        }
        (None, Some(needed)) => needed.to_string(),
        (None, None) => {
            return Err(ProgramLinkError::InvalidLibrary(format!(
                "shared library {what} has no DT_SONAME; give the name the loader finds it by"
            )));
        }
    };
    let exports = exports(what, &sections, bytes)?;
    ImportLibrary::new(soname, exports)
}

/// A string from the library, exactly: one that is not UTF-8 is refused rather than replaced,
/// since a replacement could make two different names one.
fn exact(what: &str, role: &str, bytes: &[u8]) -> Result<String, ProgramLinkError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        ProgramLinkError::InvalidLibrary(format!(
            "shared library {what}: {role} {:?} is not UTF-8",
            bytes.escape_ascii().to_string()
        ))
    })
}

type Sections<'data> = SectionTable<'data, FileHeader64<LittleEndian>>;

fn soname<'data>(
    sections: &Sections<'data>,
    bytes: &'data [u8],
) -> object::Result<Option<&'data [u8]>> {
    let table = sections.dynamic_table(LittleEndian, bytes)?;
    for entry in &table {
        if entry.tag == DT_SONAME {
            return Ok(Some(table.string(entry)?));
        }
    }
    Ok(None)
}

/// Every symbol the library lets another object bind to: defined, global or weak, visible, and
/// either unversioned or at its default version. A hidden version (`name@VERSION`, one kept for
/// binaries linked against an older library) is not what a new reference binds to, so it is
/// left out and the default one stands for the name.
fn exports(
    what: &str,
    sections: &Sections,
    bytes: &[u8],
) -> Result<Vec<(String, Export)>, ProgramLinkError> {
    let malformed =
        |error: object::Error| ProgramLinkError::Parse(format!("shared library {what}: {error}"));
    let symbols = sections
        .symbols(LittleEndian, bytes, SHT_DYNSYM)
        .map_err(malformed)?;
    let versions = sections.versions(LittleEndian, bytes).map_err(malformed)?;
    let mut exports = Vec::new();
    for (index, symbol) in symbols.enumerate() {
        if symbol.is_undefined(LittleEndian)
            || !matches!(symbol.st_bind(), STB_GLOBAL | STB_WEAK)
            || symbol.st_visibility() != object::elf::STV_DEFAULT
        {
            continue;
        }
        let name = exact(
            what,
            "symbol name",
            symbol
                .name(LittleEndian, symbols.strings())
                .map_err(malformed)?,
        )?;
        let version = match &versions {
            None => None,
            Some(table) => {
                let versym = table.version_index(LittleEndian, SymbolIndex(index.0));
                if versym.is_hidden() {
                    continue;
                }
                match versym.index().0 {
                    // VER_NDX_LOCAL: not exported, whatever the binding says.
                    0 => continue,
                    // VER_NDX_GLOBAL: the unversioned base.
                    1 => None,
                    // Any other index must name one of the library's own version definitions
                    // (not a version it requires of another library). One that names nothing is
                    // broken metadata, not an unversioned export.
                    index => match table
                        .version(versym.index())
                        .ok()
                        .flatten()
                        .filter(|version| version.file().is_none())
                    {
                        Some(version) => Some(exact(what, "version name", version.name())?),
                        None => {
                            return Err(ProgramLinkError::Parse(format!(
                                "shared library {what}: `{name}` has version index {index}, \
                                 which names no version the library defines"
                            )));
                        }
                    },
                }
            }
        };
        let kind = match symbol.st_type() {
            object::elf::STT_FUNC | object::elf::STT_GNU_IFUNC => ExportKind::Function,
            _ => ExportKind::Data,
        };
        exports.push((name, Export { kind, version }));
    }
    Ok(exports)
}

#[cfg(test)]
mod tests {
    use object::elf::{
        FileFlags, Machine, SymbolInfo, VersymIndex, DT_NULL, ELFOSABI_NONE, SHN_UNDEF, STT_FUNC,
        STV_DEFAULT,
    };
    use object::write::elf::{FileHeader, Sym, Writer};
    use object::Endianness;

    use super::*;

    /// A minimal x86-64 shared object: a `DT_SONAME` when `soname` is given, and one defined
    /// global function per name in `functions`, unversioned.
    fn shared_object(soname: Option<&[u8]>, functions: &[&[u8]]) -> Vec<u8> {
        versioned_shared_object(soname, functions, None)
    }

    /// [`shared_object`], with a `.gnu.version` table giving every function `version_index`
    /// when one is given, and no version definitions.
    fn versioned_shared_object(
        soname: Option<&[u8]>,
        functions: &[&[u8]],
        version_index: Option<u16>,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut writer = Writer::new(Endianness::Little, true, &mut bytes);
        writer.reserve_file_header();
        writer.reserve_null_section_index();
        let dynsym_section = writer.reserve_dynsym_section_index();
        writer.reserve_dynstr_section_index();
        writer.reserve_dynamic_section_index();
        if version_index.is_some() {
            writer.reserve_gnu_versym_section_index();
        }
        writer.reserve_shstrtab_section_index();
        let soname = soname.map(|soname| writer.add_dynamic_string(soname));
        writer.reserve_null_dynamic_symbol_index();
        let names: Vec<_> = functions
            .iter()
            .map(|name| {
                writer.reserve_dynamic_symbol_index();
                writer.add_dynamic_string(name)
            })
            .collect();
        writer.reserve_dynsym();
        writer.reserve_dynstr().expect("dynstr");
        writer.reserve_dynamic(usize::from(soname.is_some()) + 1);
        if version_index.is_some() {
            writer.reserve_gnu_versym();
        }
        writer.reserve_shstrtab().expect("shstrtab");
        writer.reserve_section_headers();
        writer
            .write_file_header(&FileHeader {
                os_abi: ELFOSABI_NONE,
                abi_version: 0,
                e_type: ET_DYN,
                e_machine: Machine(Arch::X86_64.elf_machine()),
                e_entry: 0,
                e_flags: FileFlags(0),
            })
            .expect("header");
        writer.write_null_dynamic_symbol();
        for name in names {
            let st_name = writer.dynamic_string_offset(Some(name));
            writer.write_dynamic_symbol(&Sym {
                section: Some(dynsym_section.0),
                st_name,
                st_info: SymbolInfo::new(STB_GLOBAL, STT_FUNC),
                st_other: STV_DEFAULT.into(),
                st_shndx: SHN_UNDEF,
                st_value: 0,
                st_size: 0,
            });
        }
        writer.write_dynstr();
        writer.write_align_dynamic();
        if let Some(soname) = soname {
            writer
                .write_dynamic_string(DT_SONAME, soname)
                .expect("soname");
        }
        writer.write_dynamic(DT_NULL, 0).expect("end");
        if let Some(version_index) = version_index {
            writer.write_null_gnu_versym();
            for _ in functions {
                writer.write_gnu_versym(VersymIndex(version_index));
            }
        }
        writer.write_shstrtab();
        writer.write_null_section_header();
        writer.write_dynsym_section_header(0, 1);
        writer.write_dynstr_section_header(0);
        writer.write_dynamic_section_header(0);
        if version_index.is_some() {
            writer.write_gnu_versym_section_header(0);
        }
        writer.write_shstrtab_section_header();
        bytes
    }

    fn read(bytes: &[u8], needed: Option<&str>) -> Result<ImportLibrary, ProgramLinkError> {
        ImportLibrary::from_elf_shared_object("libt.so", bytes, Arch::X86_64, needed)
    }

    fn function() -> Export {
        Export {
            kind: ExportKind::Function,
            version: None,
        }
    }

    #[test]
    fn a_shared_object_is_read_by_its_soname_and_exports() {
        let library = read(&shared_object(Some(b"libt.so.1"), &[b"t_open"]), None).expect("reads");
        assert_eq!(library.soname(), "libt.so.1");
        assert_eq!(library.export("t_open"), Some(&function()));
        assert_eq!(library.export("t_close"), None);
    }

    #[test]
    fn a_name_that_is_not_utf8_is_refused_rather_than_replaced() {
        assert_eq!(
            read(&shared_object(Some(b"libt.so.1"), &[b"t_\xff"]), None),
            Err(ProgramLinkError::InvalidLibrary(
                "shared library libt.so: symbol name \"t_\\\\xff\" is not UTF-8".to_string()
            ))
        );
        assert_eq!(
            read(&shared_object(Some(b"libt\xfe.so"), &[b"t_open"]), None),
            Err(ProgramLinkError::InvalidLibrary(
                "shared library libt.so: DT_SONAME \"libt\\\\xfe.so\" is not UTF-8".to_string()
            ))
        );
    }

    #[test]
    fn a_version_index_naming_no_definition_is_refused_rather_than_read_as_unversioned() {
        let bytes = versioned_shared_object(Some(b"libt.so.1"), &[b"t_open"], Some(2));
        assert_eq!(
            read(&bytes, None),
            Err(ProgramLinkError::Parse(
                "shared library libt.so: `t_open` has version index 2, which names no version \
                 the library defines"
                    .to_string()
            ))
        );
        let unversioned = versioned_shared_object(Some(b"libt.so.1"), &[b"t_open"], Some(1));
        assert_eq!(
            read(&unversioned, None).expect("reads").export("t_open"),
            Some(&function())
        );
    }

    #[test]
    fn a_library_without_a_soname_needs_its_loader_name_from_the_caller() {
        let bytes = shared_object(None, &[b"t_open"]);
        assert_eq!(
            read(&bytes, None),
            Err(ProgramLinkError::InvalidLibrary(
                "shared library libt.so has no DT_SONAME; give the name the loader finds it by"
                    .to_string()
            ))
        );
        assert_eq!(
            read(&bytes, Some("lib/libt.so")).expect("reads").soname(),
            "lib/libt.so"
        );
    }

    #[test]
    fn a_given_loader_name_may_only_repeat_the_soname() {
        let bytes = shared_object(Some(b"libt.so.1"), &[b"t_open"]);
        assert_eq!(
            read(&bytes, Some("libt.so.1")).expect("reads").soname(),
            "libt.so.1"
        );
        assert_eq!(
            read(&bytes, Some("libt.so")),
            Err(ProgramLinkError::InvalidLibrary(
                "shared library libt.so is named \"libt.so.1\" by its DT_SONAME, not \"libt.so\""
                    .to_string()
            ))
        );
    }

    #[test]
    fn construction_refuses_names_the_loader_would_not_see_exactly() {
        let refused = |soname: &str, name: &str, version: Option<&str>| {
            ImportLibrary::new(
                soname.to_string(),
                [(
                    name.to_string(),
                    Export {
                        kind: ExportKind::Function,
                        version: version.map(str::to_string),
                    },
                )],
            )
            .expect_err("refused")
            .to_string()
        };
        assert_eq!(
            refused("", "f", None),
            "library name is empty in library \"\""
        );
        assert_eq!(
            refused("libt.so\0x", "f", None),
            "library name \"libt.so\\0x\" in library \"libt.so\\0x\" contains NUL"
        );
        assert_eq!(
            refused("libt.so", "f\0g", None),
            "exported name \"f\\0g\" in library \"libt.so\" contains NUL"
        );
        assert_eq!(
            refused("libt.so", "", None),
            "exported name is empty in library \"libt.so\""
        );
        assert_eq!(
            refused("libt.so", "f", Some("V\0")),
            "version of `f` \"V\\0\" in library \"libt.so\" contains NUL"
        );
    }

    #[test]
    fn construction_refuses_a_name_exported_twice() {
        assert_eq!(
            ImportLibrary::new(
                "libt.so".to_string(),
                [("f".to_string(), function()), ("f".to_string(), function())],
            ),
            Err(ProgramLinkError::InvalidLibrary(
                "libt.so exports `f` twice".to_string()
            ))
        );
    }
}
