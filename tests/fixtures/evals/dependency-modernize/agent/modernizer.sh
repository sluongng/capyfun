#!/bin/sh
# Mock "modernizer" agent (fixture/mock). Demonstrates the verify->retry loop:
#  - first attempt: adds Last() with an off-by-one bug (returns s[0]); `go test`
#    fails, and VerifyingRunner feeds that failure back into the prompt.
#  - retry: the prompt now contains "VERIFIER FAILED", so the agent fixes the
#    existing implementation in place to return the final element.
# A real agent would reason from the test failure; the fixture branches on the
# fed-back verifier output deterministically.
set -eu
if echo "${CAPYFUN_AGENT_PROMPT:-}" | grep -q "VERIFIER FAILED"; then
	sed -i 's|return s\[0\] // BUG.*|return s[len(s)-1]|' slices.go
else
	cat >> slices.go <<'GO'

// Last returns the final element of s.
func Last[T any](s []T) T {
	return s[0] // BUG: returns the first element, not the last
}
GO
fi
