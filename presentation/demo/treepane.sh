#!/usr/bin/env bash
#
# Left-pane view for the asciinema demo: a live snapshot of the monorepo's
# third_party/widget subtree + the two fields the transforms touch. Run under
# `watch` so it refreshes as the right pane imports/transforms the tree.
#
# Usage: treepane.sh <monorepo-dir>
MONO="${1:?}"
cd "$MONO" 2>/dev/null || exit 0

printf '\033[1;33m  MONOREPO\033[0m  \033[2mthird_party/widget  (live)\033[0m\n\n'
tree -C --noreport third_party/widget 2>/dev/null | sed 's/^/  /'

echo
printf '  \033[1;36mlib/widget.go import line:\033[0m\n'
if [ -f third_party/widget/lib/widget.go ]; then
    grep -n '^import' third_party/widget/lib/widget.go | sed 's/^/    \033[32m/; s/$/\033[0m/'
elif [ -f third_party/widget/pkg/widget.go ]; then
    grep -n '^import' third_party/widget/pkg/widget.go | sed 's/^/    \033[31m/; s/$/\033[0m/'
else
    printf '    \033[2m(not imported yet)\033[0m\n'
fi

echo
printf '  \033[1;36mgo.mod toolchain:\033[0m\n'
if [ -f third_party/widget/go.mod ]; then
    grep -nE 'toolchain|^go ' third_party/widget/go.mod | sed 's/^/    \033[32m/; s/$/\033[0m/'
else
    printf '    \033[2m(not imported yet)\033[0m\n'
fi
