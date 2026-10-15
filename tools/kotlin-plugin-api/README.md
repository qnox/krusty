# Kotlin compiler plugin API surface

A jar that stands in for `kotlin-compiler-embeddable` as far as compiler plugins are concerned. It
holds exactly the part of the compiler that real plugin jars link against, and no compiler: a
plugin built for kotlinc loads, links and passes bytecode verification with this jar and the Kotlin
standard library beside it. What a plugin then asks of the compiler (sessions, symbol lookup,
builders, IR trees) is supplied by an implementation of the surface, such as a krusty-backed host.

See `docs/PLUGIN_API.md` ("Hosting kotlinc plugin jars") for where this fits in krusty.

## What is in the jar

The surface is derived, never written by hand. `SurfaceGenerator` scans every class of the given
plugin jars, resolves each type and member they reference in the reference compiler exactly as the
JVM would, and writes:

- **Contract types**: every compiler type a plugin names, with the compiler's binary shape (name,
  interface or class, abstract or final, supertypes), because plugin bytecode fixes it:
  `invokeinterface` needs an interface, `extends` a non-final class. A contract type keeps only the
  members plugins use, plus everything a plugin can override on the types plugins extend.
  - Abstract members stay abstract: an implementation supplies them by subclassing, as kotlinc does.
  - Members whose code is self-contained (it reads and writes its own state, calls the JDK, the
    standard library and data classes, and chains to other such members) keep the compiler's code:
    constructors, simple accessors, companion objects, extension-point descriptors.
  - Every other concrete member throws `UnsupportedOperationException` naming itself. That member
    is a compiler service an implementation provides.
- **Data classes**: compiler classes whose whole dependency closure needs only the JDK, the standard
  library and other data classes (names and ids, predicates, origins, cone types, metadata
  messages). They are copied whole, because they carry no compiler.
- `krusty/pluginapi/Surface`, the runtime support the stubs call.

For Kotlin 2.4.20 and the plugins below that is 942 contract types with 5,222 members and 2,658
data classes in a 5 MB jar; `build/reports/kotlin-plugin-api-surface-<kotlin>.md` has the numbers
for a build, and the jar is byte-for-byte reproducible.

A contract type's static state is real only when its initializer is self-contained. When it calls
a service (a diagnostics factory built through the compiler, for example), the field stays unset and
an implementation provides it; the load test accepts an initializer stopping there.

## Reference plugins

The surface covers the links of every first-party plugin Gradle builds load (all-open, no-arg,
sam-with-receiver, assignment, lombok, power-assert, parcelize, serialization, atomicfu, compose),
in their `-embeddable` variants, plus Poko and Metro. A plugin outside that set may name members
none of these use; add it to `build.gradle.kts` and the surface grows to cover it.

## Build and test

```sh
./gradlew surfaceJar                      # build/libs/kotlin-plugin-api-surface-<kotlin>.jar
./gradlew test                            # links, loads and verifies every reference plugin
./gradlew test -PkotlinVersion=2.4.0      # the same against another Kotlin release
```

The tests check, per reference plugin:

- every link the reference compiler satisfies, the surface satisfies (`LinkageCheck`, the JVM's
  resolution rules applied to each instruction);
- every plugin class loads, verifies and initializes with only the surface and the standard library,
  where an initializer may stop at a member an implementation provides, but never at a linkage
  error.

Links the reference compiler cannot satisfy either (Metro's compatibility code for other Kotlin
versions, for example) are counted, not failed: that code never runs against this compiler.

`LinkageCheck` also runs on its own:

```sh
java -cp <classes>:<asm> krusty.pluginapi.LinkageCheck --surface <surface.jar> \
    --compiler <kotlin-compiler-embeddable.jar> --stdlib <kotlin-stdlib.jar> <plugin.jar>...
```
