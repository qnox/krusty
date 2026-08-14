#!/usr/bin/env bash

# Run one command in its own process group. On timeout terminate the whole group, including JVMs
# spawned by a test binary, and return the conventional timeout status 124.
run_with_deadline() {
  local seconds="$1"
  shift
  command -v perl >/dev/null 2>&1 || {
    echo "test deadline: perl is required" >&2
    return 2
  }
  perl -MPOSIX -e '
    my $seconds = shift @ARGV;
    my $pid = fork();
    die "fork failed: $!\n" unless defined $pid;
    if ($pid == 0) {
      POSIX::setpgid(0, 0) == 0 or die "setpgid failed: $!\n";
      exec @ARGV;
      die "exec failed: $!\n";
    }
    my $timed_out = 0;
    $SIG{ALRM} = sub {
      $timed_out = 1;
      kill "TERM", -$pid;
      select undef, undef, undef, 0.2;
      kill "KILL", -$pid;
    };
    alarm $seconds;
    waitpid($pid, 0);
    alarm 0;
    exit 124 if $timed_out;
    exit(128 + ($? & 127)) if $? & 127;
    exit($? >> 8);
  ' "$seconds" "$@"
}
export -f run_with_deadline
