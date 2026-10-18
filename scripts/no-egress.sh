#!/bin/sh
# No-egress invariant, enforced by the OS (Linux only; needs strace and iproute2).
#
# Runs the invariant tests (startup + turns with the whole defaults bundle installed) in a
# network namespace that has only loopback, tracing every connect() from every thread.
# Fails if anything connects anywhere but 127.0.0.1 or ::1, DNS lookups included.
# The in-process version of this test lives in crates/cutoffharness-core/tests/invariants.rs.
set -eu

bin=$(cargo test -p cutoffharness-core --test invariants --no-run --locked --message-format=json \
    | sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' | grep invariants | tail -n 1)
log=$(mktemp)

# Unprivileged namespaces are disabled on some distributions (Ubuntu 24.04+); fall back to sudo.
if unshare -rn true 2>/dev/null; then ns="unshare -rn"; else ns="sudo unshare -n"; fi
$ns sh -c "ip link set lo up && strace -f -qq -e trace=connect -o '$log' '$bin' --test-threads 1"

total=$(grep -c 'connect(' "$log" || true)
bad=$(grep -E 'sa_family=AF_INET6?,' "$log" \
    | grep -v -E 'inet_addr\("127\.0\.0\.1"\)|inet_pton\(AF_INET6, "::1"' || true)
if [ -n "$bad" ]; then
    echo "no-egress: unexpected connections:" >&2
    echo "$bad" >&2
    exit 1
fi
echo "no-egress: $total connect() calls, all to loopback"
