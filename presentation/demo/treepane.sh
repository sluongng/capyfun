#!/usr/bin/env bash
#
# Left-pane view for the asciinema demo: a live snapshot of the monorepo's
# third_party/widget subtree + the two fields the transforms touch. Run under
# `watch` so it refreshes as the right pane imports/transforms the tree.
#
# Colors are built with printf (real ESC bytes) and passed to printf statements,
# never embedded in a `sed` replacement — sed parses `\033` as `\0`+`33` and
# would emit a literal "33[32m".
#
# Usage: treepane.sh <monorepo-dir>
MONO="${1:?}"
cd "$MONO" 2>/dev/null || exit 0

E=$(printf '\033')
DIM="${E}[2m"; YEL="${E}[1;33m"; CYN="${E}[1;36m"; GRN="${E}[32m"; RED="${E}[31m"; RST="${E}[0m"

emit() { # color, file, pattern: print matching lines, indented and colored
    local color="$1" file="$2" pat="$3" line
    while IFS= read -r line; do
        printf '    %s%s%s\n' "$color" "$line" "$RST"
    done < <(grep -nE "$pat" "$file")
}

printf '%s  MONOREPO%s  %sthird_party/widget  (live)%s\n\n' "$YEL" "$RST" "$DIM" "$RST"
tree -C --noreport third_party/widget 2>/dev/null | sed 's/^/  /'

W=third_party/widget/lib/widget.go
echo
if [ -f "$W" ]; then
    printf '  %slib/widget.go — first line (🤖 agent):%s\n' "$CYN" "$RST"
    printf '    %s%s%s\n' "$GRN" "$(head -1 "$W")" "$RST"
    printf '  %simport (replace scrubbed):%s\n' "$CYN" "$RST"
    emit "$GRN" "$W" '^import'
elif [ -f third_party/widget/pkg/widget.go ]; then
    printf '  %spkg/widget.go import line:%s\n' "$CYN" "$RST"
    emit "$RED" third_party/widget/pkg/widget.go '^import'
else
    printf '  %slib/widget.go:%s\n' "$CYN" "$RST"
    printf '    %s(not imported yet)%s\n' "$DIM" "$RST"
fi

echo
printf '  %sgo.mod toolchain (tip patch):%s\n' "$CYN" "$RST"
if [ -f third_party/widget/go.mod ]; then
    emit "$GRN" third_party/widget/go.mod 'toolchain|^go '
else
    printf '    %s(not imported yet)%s\n' "$DIM" "$RST"
fi
