# Sourced by run.sh and bench.sh. Sets `toolchains` to the Gradle arguments that offer the build
# every JDK the runner installed (each `JAVA_HOME_<version>_X64`, as actions/setup-java exports it)
# as a Java toolchain. Gradle does not look for them there by itself, and a build that names its own
# toolchain sources (kotlin's `org.gradle.java.installations.fromEnv`) still finds them this way.
toolchains=()
installations=
for java_home_var in $(compgen -v JAVA_HOME_ | grep -E '^JAVA_HOME_[0-9]+_X64$' | sort -V); do
  installations=${installations:+$installations,}${!java_home_var}
done
if [ -n "$installations" ]; then
  toolchains=(-Porg.gradle.java.installations.paths="$installations")
fi
