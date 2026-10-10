# The Native backend: design

This is the design record for krusty's Native target: what it is built from, which decisions are
fixed, and how the parts still to come (C interop, more operating systems) fit them. Progress and
per-phase detail live in `docs/IMPLEMENTATION_PLAN.md` ("Native runtime", "Native linker", "Native
code generator"); each module's own doc comment covers its internals.

## The principle: Go-like

The Native backend follows Go's model: cross-platform from any host, with extremely fast
compilation, even at the expense of the generated code's runtime performance. When a choice
trades compile speed or simplicity against optimization depth, the fast and simple option wins
unless a measured workload shows the cost matters. For example, Cranelift is used rather than
LLVM, exceptions propagate through a checked slot rather than unwind tables, and C calls go
directly through the C ABI rather than through generated bridges.

## The property everything else serves: any target from any host

A user's build for any supported target, from any supported host, needs krusty and nothing else.
This means no C compiler, no system linker, no sysroot and no per-target toolchain download. This is
Go's defining build property, and krusty adopts it as a requirement rather than a feature. Each
decision below is the one that keeps it:

- **krusty owns its runtime.** The runtime (allocator, collector, object model, strings, process
  entry) is freestanding C under `src/native/runtime/`, compiled once per target when krusty
  itself is built and carried inside the compiler (`src/native/prebuilt.rs`). It uses no C library:
  it talks to the kernel directly (`krusty_sys.h`), so building it needs only
  `clang --target=<triple>`, with no target libc or headers.
- **krusty owns its code generator, and emits no C.** Checked common IR is lowered to Cranelift IR
  (`src/native/codegen/`), and Cranelift writes a relocatable object.
- **krusty owns the final link** (`src/native/linker/`). Cross-compilation stops at the linker
  if the linker is the host's.

## Decided: Cranelift, as a library

Cranelift owns instruction selection and register allocation, and only those. Object layout, the
calling convention at every runtime boundary, collector integration, exception propagation and the
link stay krusty's. The lowering sits behind `native::codegen`'s boundary, so a hand-written
backend could replace Cranelift one architecture at a time. LLVM was not chosen: it would bring
back the per-target toolchain this design exists to avoid, and its build cost dominates a fast
compiler's.

## How an exception propagates

There is no unwinder. A `throw` stores the exception in one runtime slot and returns the frame's
zero value. Every call site loads that slot and branches, either to the innermost enclosing `try`'s
dispatch or out of the frame with the slot still set, which is the propagation.

- **Table-driven unwinding** needs Cranelift emitting `.eh_frame`, the linker placing it, and a
  DWARF CFI interpreter in the runtime. That is a phase, not an increment.
- **`setjmp`/`longjmp`** is rejected on correctness. It returns twice, which Cranelift's SSA cannot
  express, so a value kept in a register across the `try` would be silently stale.

The choice stays out of the IR: `IrExpr::Try`/`IrExpr::Throw` are unchanged, so moving to
unwind tables later touches only the lowering and the runtime.

For C interop this means a Kotlin function called *from* C, such as a `staticCFunction` callback,
must not let an exception reach C. C code does not check the slot. Like Kotlin/Native, a callback
that throws terminates the program.

## The linker

The linker reads 64-bit little-endian ELF relocatables (`SHT_RELA` only) and writes an `ET_EXEC`
image at the fixed base `0x400000`, in two `PT_LOAD` segments aligned to the architecture's largest
page. The runtime is built non-PIC with a small code model, so the handful of relocation kinds each
psABI defines for that model is all it applies. The limits are listed in
`docs/IMPLEMENTATION_PLAN.md` ("Native linker").

### Importing from shared libraries: Go's split

Which programs depend on a C library at run time follows Go and its cgo mode:

- **No C library linked:** the program is static and uses no libc, like a Go binary built without
  cgo. `platform.posix` and `platform.linux` calls (sockets, `epoll`, files, clocks) are served by
  the runtime's own system-call layer, as Go's `syscall` package is. Threads are the runtime's own,
  started with `clone` on a stack the runtime maps.
- **A C library linked** (SQLite, zlib, anything a cinterop library names): that library needs
  libc itself, so the program imports libc and the library dynamically, as a Go program built with
  cgo does. Its threads must then start through `pthread_create`, so that libc has per-thread
  state for C code on every thread. The runtime's thread-start entry is the one switch between the
  two modes.
- **macOS and Windows** have no stable system-call ABI, so programs there always import
  `libSystem` or `kernel32` dynamically, as Go programs do.

Either way the link is krusty's own and needs no toolchain. Imports are linked as follows:

- **Static unless something is imported.** A program that imports nothing is the same static image
  as before. Its runtime never needs a C library.
- **The image stays non-PIE at the fixed base.** Nothing in the program or runtime is relocated at
  load time. Each imported function gets one offset-table slot, which the loader fills, and one
  stub in the executable segment that jumps through it. Any reference to the import resolves at
  link time to its stub, exactly as a reference to a defined function would. So the code generator
  and runtime need no PIC or GOT handling.
- **Bind now.** `DF_BIND_NOW`: every slot is bound before the program starts, so there is no
  lazy-binding trampoline, and a missing function fails at startup, not at its first call.
- **At the default version.** Each import records the version its library makes default
  (`name@@VERSION`) in `.gnu.version_r`. An unversioned reference would bind to the oldest
  definition, which for some C library functions (`pthread_cond_wait`, `realpath`) is a
  compatibility shim.
- **Functions only.** A variable exported by a shared library would need a copy relocation. Its
  import is refused by name.
- **The executable exports nothing.** Its hash table is empty, so its runtime's own names
  (`memcpy`, `kt_*`) never interpose on a library's.
- **The interpreter** is the target C library's dynamic loader at its psABI path
  (`NativeTarget::dynamic_loader`).

**What works today, and what is refused.** The linker side of the split is in place. The
runtime side is not yet:

- The runtime's thread start is not yet switched to `pthread_create`.
- Until it is, the linker refuses a program that imports from a shared library and can reach
  the runtime's thread-start entry (`kt_thread_start`) from its entry point, because such a
  thread would have no C library state. The runtime is linked whole, so being defined does not
  count. The runtime is compiled with one section per function, and reachability follows
  relocations section by section.
- The switch lands together with a regression that calls C from a spawned Kotlin thread.

The runtime's `_start` still runs the program. With a dynamic image, the loader has already
initialized the C library by the time `_start` is entered, so C library calls from Kotlin work.
Nothing yet runs the C library's `atexit` handlers or flushes its `stdio` buffers at exit. That
arrives with the cgo-mode runtime entry and thread start.

### What a link needs from a library: captured once, not read at every build

The linker takes each library as an `ImportLibrary`: the name the loader finds it by, and the
functions it exports with their default versions. It never reads a library file itself.

- **The facts are captured once**, when a cinterop library is made against the target's headers
  and libraries, and travel inside that klib. A program built later needs only its klibs, for any
  target from any host.
- **Reading an ELF shared object** (`ImportLibrary::from_elf_shared_object`) is one way to capture
  them, and the one used for the host. The loader name is the library's `DT_SONAME`. A library
  without one must be given the name it is linked as, since only the caller knows it.
- **Names are exact.** A library name, symbol name or version that is not UTF-8, is empty, or
  contains NUL is refused rather than normalized, because the loader matches each one byte for
  byte.
- **For the C library of each target,** krusty ships the captured facts the same way it ships the
  prebuilt runtime, so a cgo-mode program needs no target sysroot either. This is the arrangement
  Zig uses to cross-link against glibc.

## C interop

The goal is compatibility with Kotlin/Native's cinterop at the source level. Code written against
`kotlinx.cinterop`, `platform.posix`, `platform.zlib` or a `.def`-generated library compiles
unchanged.

- **Direct calls, never bridges.** Kotlin/Native's platform klibs reach C through LLVM-bitcode
  stubs (`@CCall(id = "knifunptr_…")` plus `cstubs.bc`), which krusty cannot link. Kotlin/Native's
  *direct* call mode (`cinterop -Xccall-mode direct`, the `both` default for user libraries) puts
  everything a direct call needs into the metadata:
  - `@CCall.Direct(name = "deflate")` on each function
  - `@CStruct.MemberAt(offset)` on struct fields
  - `@CStruct.VarType(size, align)` on struct companions
  - `const val`s for constant macros

  The code generator calls the named C symbol with the target's C calling convention, through
  Cranelift. krusty consumes that format and nothing else.
- **krusty makes its own cinterop libraries.** It reads a `.def` file, preprocesses and parses the
  C headers itself, computes each struct's layout for the target's ABI, and writes a klib in that
  direct-call format, with the library's link facts (`ImportLibrary`) beside the metadata.
  Kotlin/Native's `cinterop -Xccall-mode direct` is used only as the reference its output is
  compared with, in tests. The platform libraries (`platform.posix`, `platform.linux`,
  `platform.zlib`) are made the same way from Kotlin/Native's own `.def` files.
- **What direct mode cannot express**, Kotlin/Native's direct mode cannot either: a function
  defined in the `.def` file itself, and a macro that expands to an expression rather than a
  constant. `errno` is the important case: it is `(*__errno_location())`. These need code, and the
  plan is for krusty's cinterop to emit such accessors as Cranelift functions in the library
  rather than compile C.
- **Per-target ABI, never the host's.** Struct layout, integer widths (`long` is 64-bit on LP64
  Linux and macOS, 32-bit on LLP64 Windows) and how a struct is passed by value are computed for
  the target: SysV x86-64, AAPCS64 (and Apple's variant), RISC-V LP64D, and Win64.

## Other operating systems

Linux (ELF) is the only operating system today. macOS and Windows reuse everything above the
object format:

- **macOS:** a Mach-O writer with `LC_LOAD_DYLIB` and bind info for imports, ad-hoc code signing
  (required on arm64), and a runtime entry and syscall layer through `libSystem` (macOS has no
  stable raw-syscall ABI, so the runtime links `libSystem` there). Library facts come from the
  SDK's text `.tbd` stubs, so no Mac is needed to build for one.
- **Windows:** a PE/COFF writer whose import directory names DLLs (`ucrtbase.dll`,
  `kernel32.dll`, `ws2_32.dll`) directly from the captured facts, so no `.lib` import libraries
  are needed. The runtime's system layer goes through `kernel32`, and Win64 is the calling
  convention.

Each is an import writer and a runtime system layer behind the same `ImportLibrary` and
`NativeTarget` boundary, not a change to cinterop, the frontend or the code generator.
