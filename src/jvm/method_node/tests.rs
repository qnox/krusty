use std::collections::HashMap;

use super::*;
use crate::jvm::classreader::{self, ExcEntry, MethodCode, MethodLocal, C};
use crate::kt_string::KtString;

/// A constant pool that assembles into the reader's own `C` entries, so an assembled body can be
/// read straight back.
struct TestPool {
    cp: Vec<C>,
    index: HashMap<String, u16>,
    bootstrap_methods: Vec<(u16, Vec<u16>)>,
}

impl TestPool {
    fn new() -> TestPool {
        TestPool {
            cp: vec![C::Other],
            index: HashMap::new(),
            bootstrap_methods: Vec::new(),
        }
    }

    fn intern(&mut self, entry: C) -> u16 {
        let key = format!("{entry:?}");
        if let Some(&index) = self.index.get(&key) {
            return index;
        }
        let index = self.cp.len() as u16;
        let wide = matches!(entry, C::Long(_) | C::Double(_));
        self.cp.push(entry);
        if wide {
            self.cp.push(C::Other);
        }
        self.index.insert(key, index);
        index
    }

    fn utf8(&mut self, text: &str) -> u16 {
        self.intern(C::Utf8(text.to_string()))
    }

    fn name_and_type(&mut self, name: &str, desc: &str) -> u16 {
        let (name, desc) = (self.utf8(name), self.utf8(desc));
        self.intern(C::NameAndType(name, desc))
    }

    fn member(&mut self, owner: &str, name: &str, desc: &str, kind: u8) -> u16 {
        let class = self.class(owner);
        let signature = self.name_and_type(name, desc);
        self.intern(match kind {
            0 => C::Fieldref(class, signature),
            1 => C::Methodref(class, signature),
            _ => C::InterfaceMethodref(class, signature),
        })
    }

    fn handle(&mut self, handle: &Handle) -> u16 {
        let kind = match handle.kind {
            1..=4 => 0,
            _ if handle.interface => 2,
            _ => 1,
        };
        let member = self.member(&handle.owner, &handle.name, &handle.desc, kind);
        self.intern(C::MethodHandle(handle.kind, member))
    }

    /// Package an assembled body as the reader would find it in a class file of this pool.
    fn method_code(&self, code: AssembledCode) -> MethodCode {
        MethodCode {
            max_stack: code.max_stack,
            max_locals: code.max_locals,
            code: code.code,
            source_cp: self.cp.as_slice().into(),
            stackmap: None,
            handlers: code
                .exception_table
                .iter()
                .map(|&(start_pc, end_pc, handler_pc, catch_type)| ExcEntry {
                    start_pc,
                    end_pc,
                    handler_pc,
                    catch_type,
                })
                .collect(),
            locals: code
                .local_variables
                .into_iter()
                .map(|local| MethodLocal {
                    start_pc: local.start_pc,
                    length: local.length,
                    slot: local.slot,
                    name: local.name,
                    descriptor: local.desc,
                })
                .collect(),
            lines: code.line_numbers,
            source_file: None,
            defining_class: "Test".to_string(),
            dependency_source_map: None,
            bootstrap_methods: self.bootstrap_methods.clone(),
        }
    }
}

impl ConstantSink for TestPool {
    fn class(&mut self, name: &str) -> u16 {
        let name = self.utf8(name);
        self.intern(C::Class(name))
    }

    fn field(&mut self, owner: &str, name: &str, desc: &str) -> u16 {
        self.member(owner, name, desc, 0)
    }

    fn method(&mut self, owner: &str, name: &str, desc: &str, interface: bool) -> u16 {
        self.member(owner, name, desc, if interface { 2 } else { 1 })
    }

    fn constant(&mut self, constant: &Constant) -> u16 {
        match constant {
            Constant::Int(value) => self.intern(C::Integer(*value)),
            Constant::Float(bits) => self.intern(C::Float(*bits)),
            Constant::Long(value) => self.intern(C::Long(*value)),
            Constant::Double(bits) => self.intern(C::Double(*bits)),
            Constant::String(value) => {
                let text = match value.as_str() {
                    Some(text) => C::Utf8(text.to_string()),
                    None => C::Utf8Units(value.units().collect()),
                };
                let text = self.intern(text);
                self.intern(C::String(text))
            }
            Constant::Class(name) => self.class(name),
            Constant::MethodType(desc) => {
                let desc = self.utf8(desc);
                self.intern(C::MethodType(desc))
            }
            Constant::Handle(handle) => self.handle(handle),
        }
    }

    fn invoke_dynamic(
        &mut self,
        name: &str,
        desc: &str,
        bootstrap: &Handle,
        arguments: &[Constant],
    ) -> u16 {
        let handle = self.handle(bootstrap);
        let arguments = arguments
            .iter()
            .map(|argument| self.constant(argument))
            .collect();
        self.bootstrap_methods.push((handle, arguments));
        let entry = self.bootstrap_methods.len() as u16 - 1;
        let signature = self.name_and_type(name, desc);
        self.intern(C::InvokeDynamic(entry, signature))
    }
}

/// Assemble `node` against a fresh pool and read it back.
fn round_trip(node: &MethodNode) -> (AssembledCode, MethodNode) {
    let mut pool = TestPool::new();
    let assembled = node.assemble(&mut pool).expect("assemble");
    let reread = MethodNode::read(
        node.access,
        &node.name,
        &node.desc,
        &pool.method_code(assembled.clone()),
    )
    .expect("read back");
    (assembled, reread)
}

#[test]
fn encodings_follow_asm_and_survive_a_round_trip() {
    let mut node = MethodNode::new(0x0009, "f", "(I)I");
    let [start, loop_head, done, end] = [(); 4].map(|_| node.new_label());
    node.nodes = vec![
        Node::Label(start),
        Node::Line { line: 7, start },
        Node::Insn(Insn::Var { op: 0x15, slot: 0 }),
        Node::Insn(Insn::Var {
            op: 0x36,
            slot: 300,
        }),
        Node::Insn(Insn::Iinc {
            slot: 300,
            delta: 1000,
        }),
        Node::Insn(Insn::Iinc { slot: 2, delta: -1 }),
        Node::Label(loop_head),
        Node::Insn(Insn::Ldc(Constant::String(KtString::from("x")))),
        Node::Insn(Insn::Ldc(Constant::Long(5))),
        Node::Insn(Insn::Op(0x58)),
        Node::Insn(Insn::Op(0x57)),
        Node::Insn(Insn::Var { op: 0x15, slot: 0 }),
        Node::Insn(Insn::TableSwitch {
            low: 1,
            high: 2,
            default: done,
            labels: vec![loop_head, done],
        }),
        Node::Label(done),
        Node::Insn(Insn::Method {
            op: 0xb9,
            owner: "sample/Counter".to_string(),
            name: "count".to_string(),
            desc: "()I".to_string(),
            interface: true,
        }),
        Node::Insn(Insn::Op(0xac)),
        Node::Label(end),
    ];
    node.local_variables = vec![LocalVariable {
        name: "x".to_string(),
        desc: "I".to_string(),
        start,
        end,
        slot: 0,
    }];
    let (assembled, reread) = round_trip(&node);
    assert_eq!(reread, node);
    // iload_0; wide istore 300; wide iinc 300 1000; iinc 2 -1; ldc; ldc2_w; pop2; pop; iload_0;
    // tableswitch at 22 (one pad byte); invokeinterface with count 1; ireturn.
    assert_eq!(
        &assembled.code[..14],
        &[0x1a, 0xc4, 0x36, 0x01, 0x2c, 0xc4, 0x84, 0x01, 0x2c, 0x03, 0xe8, 0x84, 0x02, 0xff]
    );
    assert_eq!(assembled.code[14], 0x12);
    assert_eq!(assembled.code[16], 0x14);
    assert_eq!(&assembled.code[19..22], &[0x58, 0x57, 0x1a]);
    assert_eq!(&assembled.code[22..24], &[0xaa, 0x00]);
    assert_eq!(assembled.line_numbers, vec![(0, 7)]);
    assert_eq!(
        assembled.local_variables,
        vec![AssembledLocal {
            start_pc: 0,
            length: assembled.code.len() as u16,
            slot: 0,
            name: "x".to_string(),
            desc: "I".to_string(),
        }]
    );
}

#[test]
fn a_label_no_node_places_is_reported() {
    let mut node = MethodNode::new(0x0009, "f", "()V");
    let nowhere = node.new_label();
    node.nodes = vec![Node::Insn(Insn::Jump {
        op: 0xa7,
        target: nowhere,
    })];
    assert_eq!(
        node.assemble(&mut TestPool::new()),
        Err(AssembleError::UnplacedLabel(nowhere))
    );
}

#[test]
fn long_branches_are_widened_without_a_second_emission_path() {
    let mut jump = MethodNode::new(0x0009, "f", "()V");
    let target = jump.new_label();
    jump.nodes.push(Node::Insn(Insn::Jump { op: 0xa7, target }));
    jump.nodes
        .extend((0..40_000).map(|_| Node::Insn(Insn::Op(0x00))));
    jump.nodes
        .extend([Node::Label(target), Node::Insn(Insn::Op(0xb1))]);

    let (assembled, reread) = round_trip(&jump);
    assert_eq!(assembled.code[0], 0xc8); // goto_w
    assert_eq!(
        i32::from_be_bytes(assembled.code[1..5].try_into().unwrap()),
        40_005
    );
    assert_eq!(reread, jump);

    let mut conditional = MethodNode::new(0x0009, "g", "()V");
    let target = conditional.new_label();
    conditional.nodes.push(Node::Insn(Insn::Jump {
        op: 0x99, // ifeq
        target,
    }));
    conditional
        .nodes
        .extend((0..40_000).map(|_| Node::Insn(Insn::Op(0x00))));
    conditional
        .nodes
        .extend([Node::Label(target), Node::Insn(Insn::Op(0xb1))]);

    let mut pool = TestPool::new();
    let assembled = conditional
        .assemble(&mut pool)
        .expect("assemble conditional");
    assert_eq!(&assembled.code[..4], &[0x9a, 0, 8, 0xc8]); // ifne +8; goto_w
    assert_eq!(
        i32::from_be_bytes(assembled.code[4..8].try_into().unwrap()),
        40_005
    );
    MethodNode::read(
        conditional.access,
        &conditional.name,
        &conditional.desc,
        &pool.method_code(assembled),
    )
    .expect("the widened conditional remains valid bytecode");
}

#[test]
fn branch_relaxation_recomputes_switch_padding() {
    let mut node = MethodNode::new(0x0009, "f", "()V");
    let target = node.new_label();
    node.nodes.extend([
        Node::Insn(Insn::Jump {
            op: 0x99, // ifeq
            target,
        }),
        Node::Insn(Insn::Op(0x03)), // iconst_0
        Node::Insn(Insn::TableSwitch {
            low: 0,
            high: 0,
            default: target,
            labels: vec![target],
        }),
    ]);
    node.nodes
        .extend((0..40_000).map(|_| Node::Insn(Insn::Op(0x00))));
    node.nodes
        .extend([Node::Label(target), Node::Insn(Insn::Op(0xb1))]);

    let mut pool = TestPool::new();
    let assembled = node.assemble(&mut pool).expect("assemble");
    assert_eq!(&assembled.code[..4], &[0x9a, 0, 8, 0xc8]);
    assert_eq!(assembled.code[9], 0xaa); // switch moved from pc 4 to pc 9 after widening
    assert_eq!(
        i32::from_be_bytes(assembled.code[4..8].try_into().unwrap()),
        40_025
    );
    MethodNode::read(
        node.access,
        &node.name,
        &node.desc,
        &pool.method_code(assembled),
    )
    .expect("branch and switch layout reaches a fixed point");
}

#[test]
fn malformed_control_flow_nodes_are_rejected() {
    let mut duplicate = MethodNode::new(0x0009, "f", "()V");
    let label = duplicate.new_label();
    duplicate.nodes = vec![
        Node::Label(label),
        Node::Label(label),
        Node::Insn(Insn::Op(0xb1)),
    ];
    assert_eq!(
        duplicate.assemble(&mut TestPool::new()),
        Err(AssembleError::DuplicateLabel(label))
    );

    let mut table = MethodNode::new(0x0009, "f", "()V");
    let target = table.new_label();
    table.nodes = vec![
        Node::Insn(Insn::TableSwitch {
            low: 1,
            high: 2,
            default: target,
            labels: vec![target],
        }),
        Node::Label(target),
        Node::Insn(Insn::Op(0xb1)),
    ];
    assert_eq!(
        table.assemble(&mut TestPool::new()),
        Err(AssembleError::InvalidTableSwitch {
            low: 1,
            high: 2,
            labels: 1,
        })
    );

    let mut lookup = MethodNode::new(0x0009, "f", "()V");
    let target = lookup.new_label();
    lookup.nodes = vec![
        Node::Insn(Insn::LookupSwitch {
            default: target,
            keys: vec![2, 1],
            labels: vec![target, target],
        }),
        Node::Label(target),
        Node::Insn(Insn::Op(0xb1)),
    ];
    assert_eq!(
        lookup.assemble(&mut TestPool::new()),
        Err(AssembleError::InvalidLookupSwitch)
    );
}

#[test]
fn a_branch_into_an_instruction_is_malformed() {
    let pool = TestPool::new();
    let mut body = pool.method_code(AssembledCode {
        max_stack: 1,
        max_locals: 0,
        // goto +1 (into its own operand); return
        code: vec![0xa7, 0x00, 0x01, 0xb1],
        exception_table: Vec::new(),
        line_numbers: Vec::new(),
        local_variables: Vec::new(),
        label_offsets: Vec::new(),
    });
    assert_eq!(
        MethodNode::read(0x0009, "f", "()V", &body),
        Err(MalformedCode {
            offset: 0,
            reason: "branch into the middle of an instruction",
        })
    );
    body.code = vec![0xfe, 0x00, 0x00, 0x00, 0xb1];
    assert_eq!(
        MethodNode::read(0x0009, "f", "()V", &body),
        Err(MalformedCode {
            offset: 0,
            reason: "coroutine-site marker in a class-file body",
        })
    );
}

/// Every method body in the Kotlin standard library reads into a node, lays out again, and reads
/// back to the same node; laying the re-read node out again gives the same bytes. The stdlib holds
/// every construct an inline body is compiled to, including switches, `invokedynamic`, handlers,
/// and debug tables.
#[test]
fn every_stdlib_method_round_trips() {
    let Some(stdlib) = crate::toolchain::stdlib_jar() else {
        return;
    };
    let mut archive =
        zip::ZipArchive::new(std::fs::File::open(&stdlib).expect("open stdlib")).expect("zip");
    let mut methods = 0usize;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("entry");
        if !entry.name().ends_with(".class") || entry.name().starts_with("META-INF/") {
            continue;
        }
        let class_name = entry.name().to_string();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).expect("read class");
        let class = classreader::parse_class(&bytes).expect("parse class");
        for method in &class.methods {
            let Some(body) =
                classreader::read_method_code(&bytes, &method.name, &method.descriptor)
            else {
                continue;
            };
            let context = format!("{class_name} {}{}", method.name, method.descriptor);
            let node = MethodNode::read(method.access, &method.name, &method.descriptor, &body)
                .unwrap_or_else(|error| panic!("{context}: {error:?}"));
            assert_eq!(
                node.instructions().count(),
                crate::jvm::inline::disassemble(&body.code)
                    .expect("disassemble")
                    .len(),
                "{context}"
            );
            let (assembled, reread) = round_trip(&node);
            assert_eq!(reread, node, "{context}");
            let (again, _) = round_trip(&reread);
            assert_eq!(again, assembled, "{context}");
            methods += 1;
        }
    }
    assert!(methods > 10_000, "only {methods} stdlib methods were read");
}

/// The test pool seen as a pool a body is being written into, rather than as a class file's.
impl ConstantPoolView for TestPool {
    fn entry(&self, index: u16) -> Option<PoolEntry<'_>> {
        super::pool::class_file_entry(self.cp.get(usize::from(index))?)
    }

    fn bootstrap_method(&self, index: u16) -> Option<(u16, &[u16])> {
        self.bootstrap_methods
            .get(usize::from(index))
            .map(|(handle, arguments)| (*handle, arguments.as_slice()))
    }
}

fn invoke_static(name: &str) -> Insn {
    Insn::Method {
        op: 0xb8,
        owner: "fixture/Owner".to_string(),
        name: name.to_string(),
        desc: "()V".to_string(),
        interface: false,
    }
}

/// `try { call() } catch (e: Throwable) { throw e }` with `x` live to the end of the code, whose
/// protected range and local both end at the code's last byte.
fn guarded_body() -> MethodNode {
    let mut node = MethodNode::new(0x0009, "f", "()V");
    let [start, guarded_end, handler, end] = [(); 4].map(|_| node.new_label());
    node.nodes = vec![
        Node::Label(start),
        Node::Line { line: 3, start },
        Node::Insn(invoke_static("call")),
        Node::Insn(Insn::Op(0xb1)),
        Node::Label(handler),
        Node::Insn(Insn::Op(0xbf)),
        Node::Label(guarded_end),
        Node::Label(end),
    ];
    node.try_catch_blocks = vec![TryCatchBlock {
        start,
        end: guarded_end,
        handler,
        catch_type: Some("java/lang/Throwable".to_string()),
    }];
    node.local_variables = vec![LocalVariable {
        name: "x".to_string(),
        desc: "I".to_string(),
        start,
        end,
        slot: 0,
    }];
    node
}

#[test]
fn a_body_reads_against_the_pool_it_was_written_into() {
    let node = guarded_body();
    let mut pool = TestPool::new();
    let assembled = node.assemble(&mut pool).expect("assemble");
    let body = pool.method_code(assembled);
    let from_class_file = MethodNode::read(node.access, &node.name, &node.desc, &body)
        .expect("read from the class file's pool");
    let from_writer = MethodNode::read_code(
        node.access,
        &node.name,
        &node.desc,
        &CodeAttribute::from(&body),
        &pool,
    )
    .expect("read from the writer's pool");
    assert_eq!(from_writer, from_class_file);
}

#[test]
fn a_range_may_end_at_the_end_of_the_code() {
    let node = guarded_body();
    let (assembled, reread) = round_trip(&node);
    // invokestatic (3 bytes), return, athrow.
    assert_eq!(assembled.code.len(), 5);
    assert_eq!(assembled.exception_table.len(), 1);
    assert_eq!(assembled.exception_table[0].1, 5);
    assert_eq!(assembled.local_variables[0].length, 5);
    // One label per distinct offset: the end of the protected range and of `x` are one label,
    // which ends the node.
    assert_eq!(
        reread.nodes.last(),
        Some(&Node::Label(reread.local_variables[0].end))
    );
    assert_eq!(
        reread.try_catch_blocks[0].end,
        reread.local_variables[0].end
    );
    let again = round_trip(&reread).0;
    assert_eq!(again.code, assembled.code);
    assert_eq!(again.exception_table, assembled.exception_table);
    assert_eq!(again.local_variables, assembled.local_variables);
}

#[test]
fn a_branch_or_line_at_the_end_of_the_code_is_malformed() {
    let pool = TestPool::new();
    let mut body = pool.method_code(AssembledCode {
        max_stack: 0,
        max_locals: 0,
        // return; goto +0 is fine, goto +3 would leave the code.
        code: vec![0xb1, 0xa7, 0x00, 0x03],
        exception_table: Vec::new(),
        line_numbers: Vec::new(),
        local_variables: Vec::new(),
        label_offsets: Vec::new(),
    });
    assert_eq!(
        MethodNode::read(0x0009, "f", "()V", &body),
        Err(MalformedCode {
            offset: 1,
            reason: "branch past the last instruction",
        })
    );
    body.code = vec![0xb1];
    body.lines = vec![(1, 7)];
    assert_eq!(
        MethodNode::read(0x0009, "f", "()V", &body),
        Err(MalformedCode {
            offset: 1,
            reason: "line number past the last instruction",
        })
    );
}

#[test]
fn label_offsets_account_for_a_widened_branch() {
    let mut node = MethodNode::new(0x0009, "f", "()V");
    let target = node.new_label();
    node.nodes = vec![Node::Insn(Insn::Jump { op: 0xa7, target })];
    node.nodes
        .extend((0..32_764).map(|_| Node::Insn(Insn::Op(0x00))));
    node.nodes.push(Node::Label(target));
    node.nodes.push(Node::Insn(Insn::Op(0xb1)));
    // `goto` at 0 reaches offset 32767, the farthest a two-byte delta can.
    let reach = node.assemble(&mut TestPool::new()).expect("in range");
    assert_eq!(reach.code[0], 0xa7);
    assert_eq!(reach.offset_of(target), Some(32_767));
    // One more byte widens it to the five-byte `goto_w`, moving the label by two.
    node.nodes.insert(1, Node::Insn(Insn::Op(0x00)));
    let widened = node.assemble(&mut TestPool::new()).expect("widened");
    assert_eq!(widened.code[0], 0xc8);
    assert_eq!(widened.offset_of(target), Some(32_770));
}

#[test]
fn an_instruction_without_labels_encodes_and_decodes_alone() {
    let mut pool = TestPool::new();
    let call = invoke_static("call");
    let bytes = call
        .encode_in_place(&mut pool)
        .expect("encode")
        .expect("a call does not depend on where it stands");
    assert_eq!(bytes[0], 0xb8);
    assert_eq!(Insn::decode(&bytes, &pool), Ok(call));
    let mut node = MethodNode::new(0x0009, "f", "()V");
    let jump = Insn::Jump {
        op: 0xa7,
        target: node.new_label(),
    };
    assert_eq!(jump.encode_in_place(&mut pool), Ok(None));
    assert_eq!(
        Insn::decode(&[0xa7, 0x00, 0x00], &pool),
        Err(MalformedCode {
            offset: 0,
            reason: "a jump or switch needs its labels",
        })
    );
    assert_eq!(
        Insn::decode(&[0xb1, 0xb1], &pool),
        Err(MalformedCode {
            offset: 0,
            reason: "not exactly one instruction",
        })
    );
}
