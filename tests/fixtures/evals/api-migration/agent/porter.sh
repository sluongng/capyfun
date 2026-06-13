#!/bin/sh
# Mock "porter" agent (fixture/mock; no model, no network). A real coding agent
# would read the incoming diff + compiler errors and migrate the caller; here we
# do the same edit deterministically so the loop is reproducible and free.
set -eu
for g in $(find . -name '*.go'); do
	sed -i 's/greeter\.Connect(/greeter.New(/g' "$g"
done
