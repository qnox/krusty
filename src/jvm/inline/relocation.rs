//! Moving a compiled body's constant-pool references into ANOTHER class, and deciding whether it
//! may move at all.
//!
//! A body read from the classpath names everything through its DEFINING class's constant pool, and
//! an `invokedynamic` names a `BootstrapMethods` entry of that class by INDEX rather than through
//! the pool at all. Splicing the body into a caller means re-interning every one of those in the
//! host — and first proving the host is allowed to reference what they reach.
//!
//! The two halves live together because they answer one question between them. Relocation is
//! mechanical: it either re-interns an entry or reports that it cannot. Eligibility is the
//! judgement, and it is the half with no second chance — the verifier does not look inside a
//! bootstrap entry, so a dependency the host may not reference throws `BootstrapMethodError` when
//! the relocated instruction first executes, long after anything could have refused it.
//!
//! The writer boundary is deliberately narrow: nothing here decides WHAT to emit. A `ClassWriter`
//! is taken only to intern, and every function reports failure rather than emitting something
//! approximate.
use super::{class_name, name_and_type, pool_operand, utf8, utf8_value, Insn};
use crate::jvm::bytecode::instruction_len;
use crate::jvm::classfile::ClassWriter;
use crate::jvm::classreader::C;

type MemberDependency<'a> = (&'a str, &'a str, &'a str);

/// Every linkage dependency carried by one bootstrap entry.
#[derive(Debug, Eq, PartialEq)]
struct BootstrapDependencies<'a> {
    members: Vec<MemberDependency<'a>>,
    classes: Vec<&'a str>,
}

fn valid_internal_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains("//")
        && !name.contains(['.', ';', '['])
}

/// The reference class named by one field descriptor, if any. Primitive scalars and primitive
/// arrays have no class dependency. The full descriptor must be consumed.
fn field_descriptor_class(descriptor: &str) -> Option<Option<&str>> {
    let base = descriptor.trim_start_matches('[');
    if base.len() == 1
        && matches!(
            base.as_bytes()[0],
            b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z'
        )
    {
        return Some(None);
    }
    let class = base.strip_prefix('L')?.strip_suffix(';')?;
    valid_internal_name(class).then_some(Some(class))
}

fn descriptor_classes(descriptor: &str, method: bool) -> Option<Vec<&str>> {
    let fields = if method {
        let (parameters, result) = crate::jvm::names::parse_method_descriptor(descriptor)?;
        parameters
            .into_iter()
            .chain((result != "V").then_some(result))
            .collect::<Vec<_>>()
    } else {
        vec![descriptor]
    };
    fields
        .into_iter()
        .try_fold(Vec::new(), |mut classes, field| {
            if let Some(class) = field_descriptor_class(field)? {
                classes.push(class);
            }
            Some(classes)
        })
}

fn class_constant_dependencies(name: &str) -> Option<Vec<&str>> {
    if name.starts_with('[') {
        return descriptor_classes(name, false);
    }
    valid_internal_name(name).then_some(vec![name])
}

fn member_dependency<'a>(
    src_cp: &'a [C],
    kind: u8,
    member: u16,
) -> Option<(MemberDependency<'a>, Vec<&'a str>)> {
    let (class, signature, method) = match (kind, src_cp.get(member as usize)?) {
        (1..=4, C::Fieldref(class, signature)) => (*class, *signature, false),
        (5 | 8, C::Methodref(class, signature)) => (*class, *signature, true),
        (6 | 7, C::Methodref(class, signature) | C::InterfaceMethodref(class, signature)) => {
            (*class, *signature, true)
        }
        (9, C::InterfaceMethodref(class, signature)) => (*class, *signature, true),
        _ => return None,
    };
    let owner = class_name(src_cp, class)?;
    if !valid_internal_name(owner) {
        return None;
    }
    let (name, descriptor) = name_and_type(src_cp, signature)?;
    let mut classes = vec![owner];
    classes.extend(descriptor_classes(descriptor, method)?);
    Some(((owner, name, descriptor), classes))
}

/// Re-intern the source constant-pool entry at `idx` (from the inline body's defining class, `src_cp`)
/// into the target class's pool (`cw`), returning the new pool index. Resolving each entry to its
/// semantic form (class/method/field names, descriptors, constant values) and re-interning is what
/// lets a body compiled against one class run inside another. `None` for an entry kind not yet
/// relocatable (`invokedynamic`/method handles — those need bootstrap-method relocation too).
pub fn relocate_const(src_cp: &[C], idx: u16, cw: &mut ClassWriter) -> Option<u16> {
    match src_cp.get(idx as usize)? {
        C::Class(n) => Some(cw.class_ref(utf8(src_cp, *n)?)),
        // A value-bearing string uses the class reader's single code-unit accessor. Names and
        // descriptors use `utf8` above; duplicating the variant conversion here risks making
        // classpath constant inlining disagree with `ConstantValue` field reads.
        C::String(u) => Some(cw.const_string_kt(&utf8_value(src_cp, *u)?)),
        C::Integer(v) => Some(cw.const_int(*v)),
        C::Float(b) => Some(cw.const_float(f32::from_bits(*b))),
        C::Long(v) => Some(cw.const_long(*v)),
        C::Double(b) => Some(cw.const_double(f64::from_bits(*b))),
        C::Methodref(c, nt) => {
            // Owned before the writer is touched: `cw` is borrowed mutably and these all borrow
            // `src_cp`, which the borrow checker will not let overlap.
            let cn = class_name(src_cp, *c)?.to_string();
            let (n, d) = name_and_type(src_cp, *nt)?;
            let (n, d) = (n.to_string(), d.to_string());
            Some(cw.methodref(&cn, &n, &d))
        }
        C::Fieldref(c, nt) => {
            // Owned before the writer is touched: `cw` is borrowed mutably and these all borrow
            // `src_cp`, which the borrow checker will not let overlap.
            let cn = class_name(src_cp, *c)?.to_string();
            let (n, d) = name_and_type(src_cp, *nt)?;
            let (n, d) = (n.to_string(), d.to_string());
            Some(cw.fieldref(&cn, &n, &d))
        }
        C::InterfaceMethodref(c, nt) => {
            // Owned before the writer is touched: `cw` is borrowed mutably and these all borrow
            // `src_cp`, which the borrow checker will not let overlap.
            let cn = class_name(src_cp, *c)?.to_string();
            let (n, d) = name_and_type(src_cp, *nt)?;
            let (n, d) = (n.to_string(), d.to_string());
            Some(cw.interface_methodref(&cn, &n, &d))
        }
        // A bootstrap method is named by a handle onto a member, and its static arguments are
        // ordinary constants plus, commonly, a `MethodType`. Both relocate through the member/utf8
        // they wrap.
        C::MethodHandle(kind, member) => {
            let (kind, member) = (*kind, *member);
            let relocated = relocate_const(src_cp, member, cw)?;
            Some(cw.method_handle_ref(kind, relocated))
        }
        C::MethodType(descriptor) => {
            let descriptor = utf8(src_cp, *descriptor)?.to_string();
            Some(cw.method_type_ref(&descriptor))
        }
        _ => None,
    }
}

/// Every member and class one `BootstrapMethods` entry reaches: the handle naming the factory, its
/// member descriptor, and every static argument.
///
/// This is the complete dependency graph an `invokedynamic` carries. Relocating the instruction
/// into another class re-interns every one of these in the HOST's pool, so the property that
/// decides whether the entry may move is structural, not a question of which factory it names: the
/// host must be allowed to reference each member and every class named by a class constant or
/// descriptor, and each entry must be a constant kind
/// [`relocate_const`] can re-intern. A `StringConcatFactory` entry reaches only its own public
/// factory, a recipe string and constants; a `LambdaMetafactory` one also names an implementation
/// handle in the DEFINING class, which is usually private and synthetic — but a concat entry may
/// equally carry a handle to an inaccessible member, which is why the answer cannot come from the
/// factory's spelling.
///
/// `None` when any reachable entry is a kind relocation cannot carry (a `CONSTANT_Dynamic`, a
/// handle onto something that is not a member, an index past the pool). Fail closed: the caller
/// cannot vouch for what it cannot read, and declining only means a real call is emitted.
fn bootstrap_dependencies<'a>(
    src_cp: &'a [C],
    handle: u16,
    arguments: &[u16],
) -> Option<BootstrapDependencies<'a>> {
    // JVMS 4.7.23: a bootstrap method is named by a `CONSTANT_MethodHandle` and nothing else. A
    // slot holding anything else is a table this walk cannot read, not an entry with no members.
    if !matches!(src_cp.get(handle as usize), Some(C::MethodHandle(6 | 8, _))) {
        return None;
    }
    let mut members = Vec::new();
    let mut classes = Vec::new();
    let mut pending: Vec<u16> = Vec::with_capacity(1 + arguments.len());
    pending.push(handle);
    pending.extend_from_slice(arguments);
    while let Some(index) = pending.pop() {
        match src_cp.get(index as usize)? {
            // A handle is the only entry that names a member the host would have to reference.
            C::MethodHandle(kind, member) => {
                let (member, member_classes) = member_dependency(src_cp, *kind, *member)?;
                members.push(member);
                classes.extend(member_classes);
            }
            C::MethodType(descriptor) => {
                classes.extend(descriptor_classes(utf8(src_cp, *descriptor)?, true)?);
            }
            C::Class(name) => {
                classes.extend(class_constant_dependencies(utf8(src_cp, *name)?)?);
            }
            // Validation must consume exactly the indirection relocation will consume. Accepting
            // the String slot alone would let a malformed payload fail only after an earlier
            // bootstrap entry had already mutated the destination writer.
            C::String(value) => {
                utf8_value(src_cp, *value)?;
            }
            C::Integer(_) | C::Float(_) | C::Long(_) | C::Double(_) => {}
            _ => return None,
        }
    }
    Some(BootstrapDependencies { members, classes })
}

/// Compatibility projection for callers interested only in member dependencies.
pub fn bootstrap_members<'a>(
    src_cp: &'a [C],
    handle: u16,
    arguments: &[u16],
) -> Option<Vec<(&'a str, &'a str, &'a str)>> {
    bootstrap_dependencies(src_cp, handle, arguments).map(|dependencies| dependencies.members)
}

/// Whether `code` references (through `src_cp`) a method/field `is_private` flags as `ACC_PRIVATE`.
/// Such a body runs legally only inside its DEFINING class: spliced into a caller, the reference is
/// an `IllegalAccessError` (kotlinc rewrites it to a synthetic `access$…` bridge — unmodelled
/// here), so the splicer must decline. A malformed body reports `true` — the caller cannot verify
/// what it cannot walk, and declining is always safe (a real call instead).
///
/// Both ways a body reaches a member are walked, and they are held to DIFFERENT standards.
///
/// An instruction's constant-pool operand need only not be `ACC_PRIVATE`: the verifier checks
/// everything else at the use site, and an `IllegalAccessError` is raised before the instruction
/// runs.
///
/// A `BootstrapMethods` entry must PROVE reachability through `is_publicly_reachable`. Its handle
/// and static arguments appear in no instruction operand. Member handles are checked through
/// `is_publicly_reachable`; class constants and every reference type in member/method-type
/// descriptors are checked through `class_is_publicly_reachable`. An entry that cannot be reached
/// throws `BootstrapMethodError` the first time the relocated instruction executes — after
/// verification, so no earlier check catches it. Anything unproven declines.
///
/// `bootstraps` is the DEFINING class's table, indexed as `CONSTANT_InvokeDynamic` indexes it.
pub fn references_private_member(
    code: &[u8],
    src_cp: &[C],
    bootstraps: &[(u16, Vec<u16>)],
    is_private: &mut dyn FnMut(&str, &str, &str) -> bool,
    is_publicly_reachable: &mut dyn FnMut(&str, &str, &str) -> bool,
    class_is_publicly_reachable: &mut dyn FnMut(&str) -> bool,
) -> bool {
    let mut pc = 0;
    while pc < code.len() {
        let Some(len) = instruction_len(code, pc) else {
            return true;
        };
        if let Some((off, width)) = pool_operand(code[pc]) {
            let idx = if width == 1 {
                u16::from(code[pc + off])
            } else {
                u16::from_be_bytes([code[pc + off], code[pc + off + 1]])
            };
            let member = match src_cp.get(idx as usize) {
                Some(C::Methodref(c, nt) | C::InterfaceMethodref(c, nt) | C::Fieldref(c, nt)) => {
                    class_name(src_cp, *c).zip(name_and_type(src_cp, *nt))
                }
                // An `invokedynamic` names a bootstrap entry rather than a member. Its whole graph
                // is walked; an unreadable one reports `true` for the same reason a malformed
                // instruction does.
                Some(C::InvokeDynamic(entry, call_site)) => {
                    let Some((handle, arguments)) = bootstraps.get(*entry as usize) else {
                        return true;
                    };
                    let Some(dependencies) = bootstrap_dependencies(src_cp, *handle, arguments)
                    else {
                        return true;
                    };
                    let Some((_, descriptor)) = name_and_type(src_cp, *call_site) else {
                        return true;
                    };
                    let Some(call_site_classes) = descriptor_classes(descriptor, true) else {
                        return true;
                    };
                    // A bootstrap entry must PROVE reachability, where an instruction operand
                    // need only not be private. The two differ for a package-private or protected
                    // member, for a public member of a package-private class, and for a member
                    // this compilation cannot see at all — and an entry that fails any of those
                    // throws at LINKAGE, so no verifier check exists to fall back on.
                    if !dependencies
                        .members
                        .into_iter()
                        .all(|(owner, name, descriptor)| {
                            is_publicly_reachable(owner, name, descriptor)
                        })
                        || !dependencies
                            .classes
                            .into_iter()
                            .chain(call_site_classes)
                            .all(&mut *class_is_publicly_reachable)
                    {
                        return true;
                    }
                    None
                }
                _ => None,
            };
            if let Some((owner, (name, descriptor))) = member {
                if is_private(owner, name, descriptor) {
                    return true;
                }
            }
        }
        pc += len;
    }
    false
}

const LDC: u8 = 0x12;
const LDC_W: u8 = 0x13;

/// An `ldc` of the one-word constant at `index` in the form ASM writes it: `ldc` when the index
/// fits one byte, `ldc_w` otherwise.
pub(super) fn narrowest_ldc(index: u16) -> Insn {
    match u8::try_from(index) {
        Ok(index) => Insn::Plain {
            op: LDC,
            operands: vec![index],
        },
        Err(_) => Insn::Plain {
            op: LDC_W,
            operands: index.to_be_bytes().to_vec(),
        },
    }
}

/// Relocate every constant-pool reference in a disassembled body into `cw`'s pool (the insn-level
/// counterpart of [`relocate_code`], so relocation composes with the local/return/reified transforms
/// before reassembly). `None` on `invokedynamic` or an unsupported one-byte pool operand. An `ldc`
/// or `ldc_w` takes the form its relocated index needs ([`narrowest_ldc`]).
pub fn relocate_insns(
    insns: &mut [Insn],
    src_cp: &[C],
    bootstraps: &[(u16, Vec<u16>)],
    cw: &mut ClassWriter,
) -> Option<()> {
    // Validate every bootstrap graph before interning anything. A later malformed entry must not
    // leave a partially relocated bootstrap or constant-pool graph in the destination writer.
    for insn in insns.iter() {
        let Insn::Plain { op: 0xba, operands } = insn else {
            continue;
        };
        let [high, low, 0, 0] = operands.as_slice() else {
            return None;
        };
        let source = u16::from_be_bytes([*high, *low]);
        let C::InvokeDynamic(bootstrap, name_and_type_index) = *src_cp.get(source as usize)? else {
            return None;
        };
        let (handle, arguments) = bootstraps.get(bootstrap as usize)?;
        bootstrap_dependencies(src_cp, *handle, arguments)?;
        let (_, descriptor) = name_and_type(src_cp, name_and_type_index)?;
        crate::jvm::names::parse_method_descriptor(descriptor)?;
    }

    for insn in insns.iter_mut() {
        let Insn::Plain { op, operands } = insn else {
            continue;
        };
        let Some((off, width)) = pool_operand(*op) else {
            continue;
        };
        if *op == 0xba {
            // invokedynamic names a `BootstrapMethods` entry of its DEFINING class by index, not a
            // constant pool entry, so relocating it means re-interning that entry here: the handle,
            // its static arguments, and the name/type. `add_bootstrap` dedupes on the host side.
            //
            // Kotlin reaches this through string concatenation, which compiles to
            // `invokedynamic makeConcatWithConstants` from JVM target 9 — a lambda inside an
            // `inline` function does not, kotlinc compiling those as anonymous-class singletons
            // precisely so an inliner can copy them.
            let o = off - 1;
            let src_idx = (*operands.get(o)? as u16) << 8 | *operands.get(o + 1)? as u16;
            let C::InvokeDynamic(bootstrap_index, name_and_type_index) =
                *src_cp.get(src_idx as usize)?
            else {
                return None;
            };
            let (handle, arguments) = bootstraps.get(bootstrap_index as usize)?;
            // The entry moves only if its COMPLETE dependency graph can move with it. Walking it
            // here rejects an unsupported constant kind before anything is interned; whether the
            // host may reference the members it reaches is decided by the splice-eligibility check
            // that owns the accessibility predicate, alongside the instruction-level one.
            bootstrap_members(src_cp, *handle, arguments)?;
            let handle = relocate_const(src_cp, *handle, cw)?;
            let arguments = arguments
                .iter()
                .map(|argument| relocate_const(src_cp, *argument, cw))
                .collect::<Option<Vec<u16>>>()?;
            let bootstrap = cw.add_bootstrap(handle, arguments);
            let (name, descriptor) = name_and_type(src_cp, name_and_type_index)?;
            let (name, descriptor) = (name.to_string(), descriptor.to_string());
            let new = cw.invoke_dynamic_ref(bootstrap, &name, &descriptor);
            *operands = vec![(new >> 8) as u8, (new & 0xff) as u8, 0, 0];
            continue;
        }
        // `off` is relative to the opcode; in `operands` (opcode stripped) it is `off - 1`.
        let o = off - 1;
        let src_idx = if width == 1 {
            *operands.get(o)? as u16
        } else {
            (*operands.get(o)? as u16) << 8 | *operands.get(o + 1)? as u16
        };
        let new = relocate_const(src_cp, src_idx, cw)?;
        if matches!(*op, LDC | LDC_W) {
            // The dependency's pool and the host's differ in size, so the form its `ldc` took says
            // nothing about the host: write the one ASM (and so kotlinc) picks for the host index.
            // The assembler derives instruction length from the opcode, so the size change is
            // handled downstream (see `old_offsets`).
            *insn = narrowest_ldc(new);
            continue;
        }
        if width == 1 {
            // `ldc` is the only instruction with a one-byte pool index.
            return None;
        }
        operands[o] = (new >> 8) as u8;
        operands[o + 1] = (new & 0xff) as u8;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dependency body loads a constant with `ldc_w` because its own pool is large; in the host
    /// the constant lands at a small index, and ASM writes `ldc` there.
    #[test]
    fn a_relocated_ldc_takes_the_form_its_host_index_needs() {
        let src_cp = vec![C::Other, C::Utf8("message".into()), C::String(1)];
        let mut cw = ClassWriter::new("T", "java/lang/Object");
        let mut insns = vec![
            Insn::Plain {
                op: LDC_W,
                operands: vec![0x00, 0x02],
            },
            Insn::Plain {
                op: LDC,
                operands: vec![0x02],
            },
        ];
        relocate_insns(&mut insns, &src_cp, &[], &mut cw).expect("relocate");
        let host = cw.const_string("message");
        assert!(host <= 0xff);
        assert_eq!(insns, vec![narrowest_ldc(host), narrowest_ldc(host)]);
        assert_eq!(
            narrowest_ldc(host),
            Insn::Plain {
                op: LDC,
                operands: vec![host as u8],
            }
        );
        assert_eq!(
            narrowest_ldc(0x123),
            Insn::Plain {
                op: LDC_W,
                operands: vec![0x01, 0x23],
            }
        );
    }

    fn bootstrap_pool(argument: C, argument_utf8: &str) -> Vec<C> {
        vec![
            C::Other,
            C::Utf8("public/Bootstrap".to_string()),
            C::Class(1),
            C::Utf8("bootstrap".to_string()),
            C::Utf8("()V".to_string()),
            C::NameAndType(3, 4),
            C::Methodref(2, 5),
            C::MethodHandle(6, 6),
            C::Utf8(argument_utf8.to_string()),
            argument,
            C::Utf8("run".to_string()),
            C::NameAndType(10, 4),
            C::InvokeDynamic(0, 11),
        ]
    }

    fn references_unreachable_class(pool: &[C]) -> bool {
        references_private_member(
            &[0xba, 0x00, 0x0c, 0x00, 0x00, 0xb1],
            pool,
            &[(7, vec![9])],
            &mut |_, _, _| false,
            &mut |_, _, _| true,
            &mut |class| class != "hidden/Type",
        )
    }

    #[test]
    fn inaccessible_class_argument_including_array_element_declines() {
        let pool = bootstrap_pool(C::Class(8), "[[Lhidden/Type;");
        assert_eq!(
            bootstrap_dependencies(&pool, 7, &[9]),
            Some(BootstrapDependencies {
                members: vec![("public/Bootstrap", "bootstrap", "()V")],
                classes: vec!["hidden/Type", "public/Bootstrap"],
            })
        );
        assert!(references_unreachable_class(&pool));
    }

    #[test]
    fn inaccessible_method_type_argument_declines() {
        let pool = bootstrap_pool(C::MethodType(8), "(Lhidden/Type;)V");
        assert_eq!(
            bootstrap_dependencies(&pool, 7, &[9]),
            Some(BootstrapDependencies {
                members: vec![("public/Bootstrap", "bootstrap", "()V")],
                classes: vec!["hidden/Type", "public/Bootstrap"],
            })
        );
        assert!(references_unreachable_class(&pool));
    }

    #[test]
    fn member_descriptor_reference_types_join_the_dependency_inventory() {
        let mut pool = bootstrap_pool(C::String(8), "recipe");
        pool[4] = C::Utf8("(Lhidden/Argument;)[Lhidden/Result;".to_string());
        assert_eq!(
            bootstrap_dependencies(&pool, 7, &[9]),
            Some(BootstrapDependencies {
                members: vec![(
                    "public/Bootstrap",
                    "bootstrap",
                    "(Lhidden/Argument;)[Lhidden/Result;"
                )],
                classes: vec!["public/Bootstrap", "hidden/Argument", "hidden/Result"],
            })
        );
    }

    #[test]
    fn malformed_bootstrap_descriptor_fails_closed() {
        let pool = bootstrap_pool(C::MethodType(8), "(Lhidden/Type)V");
        assert_eq!(bootstrap_dependencies(&pool, 7, &[9]), None);
        assert!(references_unreachable_class(&pool));
    }

    #[test]
    fn inaccessible_call_site_descriptor_type_declines() {
        let mut pool = bootstrap_pool(C::String(8), "recipe");
        pool.push(C::Utf8("(Lhidden/Type;)V".to_string()));
        pool[11] = C::NameAndType(10, 13);
        assert!(references_unreachable_class(&pool));
    }

    #[test]
    fn all_bootstrap_graphs_are_validated_before_writer_mutation() {
        let mut pool = bootstrap_pool(C::String(8), "recipe");
        pool.extend([
            C::Utf8("(Lbroken)V".to_string()),
            C::MethodType(13),
            C::InvokeDynamic(1, 11),
        ]);
        let mut instructions = vec![
            Insn::Plain {
                op: 0xba,
                operands: vec![0, 12, 0, 0],
            },
            Insn::Plain {
                op: 0xba,
                operands: vec![0, 15, 0, 0],
            },
        ];
        let mut writer = ClassWriter::new("Host", "java/lang/Object");
        assert!(relocate_insns(
            &mut instructions,
            &pool,
            &[(7, vec![9]), (7, vec![14])],
            &mut writer,
        )
        .is_none());
        assert_eq!(
            writer.finish(),
            ClassWriter::new("Host", "java/lang/Object").finish(),
            "a later malformed bootstrap must be found before the first entry mutates the writer"
        );
    }

    #[test]
    fn malformed_later_string_is_found_before_writer_mutation() {
        let mut pool = bootstrap_pool(C::String(8), "recipe");
        pool.extend([C::Integer(7), C::String(13), C::InvokeDynamic(1, 11)]);
        let mut instructions = vec![
            Insn::Plain {
                op: 0xba,
                operands: vec![0, 12, 0, 0],
            },
            Insn::Plain {
                op: 0xba,
                operands: vec![0, 15, 0, 0],
            },
        ];
        let mut writer = ClassWriter::new("Host", "java/lang/Object");
        assert!(relocate_insns(
            &mut instructions,
            &pool,
            &[(7, vec![9]), (7, vec![14])],
            &mut writer,
        )
        .is_none());
        assert_eq!(
            writer.finish(),
            ClassWriter::new("Host", "java/lang/Object").finish(),
            "a malformed later string must be found before the first entry mutates the writer"
        );
    }
}
