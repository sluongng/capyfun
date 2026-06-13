#!/usr/bin/env bash
#
# Left-pane view for an asciinema demo: the declared SRC config, the live
# third_party/widget tree, and the field(s) the transforms touch. Run under
# `watch` so it refreshes as the right pane imports/transforms the tree.
#
# Colors are real ESC bytes from printf (never embedded in a `sed` replacement,
# which would parse `\033` as `\0`+`33` and print a literal "33[32m").
#
# Usage: treepane.sh <monorepo-dir> <imperative|generative>
MONO="${1:?}"
MODE="${2:-imperative}"
cd "$MONO" 2>/dev/null || exit 0

E=$(printf '\033')
DIM="${E}[2m"; YEL="${E}[1;33m"; CYN="${E}[1;36m"; GRN="${E}[32m"; RST="${E}[0m"
SRC=third_party/widget/SRC
W=third_party/widget/lib/widget.go

# --- the declared config (so viewers always see what drives the run) --------
printf '%s  //third_party/widget/SRC%s\n' "$YEL" "$RST"
if [ -f "$SRC" ]; then
    while IFS= read -r line; do
        trimmed="${line#"${line%%[![:space:]]*}"}"
        case "$trimmed" in
            '#'*) printf '  %s%s%s\n' "$DIM" "$line" "$RST" ;;   # comment
            *)    printf '  %s\n' "$line" ;;
        esac
    done < "$SRC"
fi

# --- the resulting tree -----------------------------------------------------
echo
printf '%s  third_party/widget%s  %s(live)%s\n' "$YEL" "$RST" "$DIM" "$RST"
tree -C --noreport third_party/widget 2>/dev/null | sed 's/^/  /'

# --- what the transforms produced ------------------------------------------
echo
if [ "$MODE" = generative ]; then
    printf '  %slib/widget.go — first line (🤖 agent):%s\n' "$CYN" "$RST"
    if [ -f "$W" ]; then
        printf '    %s%s%s\n' "$GRN" "$(head -1 "$W")" "$RST"
    else
        printf '    %s(not imported yet)%s\n' "$DIM" "$RST"
    fi
else
    printf '  %slib/widget.go import (scrubbed):%s\n' "$CYN" "$RST"
    if [ -f "$W" ]; then
        while IFS= read -r line; do printf '    %s%s%s\n' "$GRN" "$line" "$RST"; done \
            < <(grep -n '^import' "$W")
    else
        printf '    %s(not imported yet)%s\n' "$DIM" "$RST"
    fi
    printf '  %sgo.mod toolchain (tip patch):%s\n' "$CYN" "$RST"
    if [ -f third_party/widget/go.mod ]; then
        while IFS= read -r line; do printf '    %s%s%s\n' "$GRN" "$line" "$RST"; done \
            < <(grep -nE 'toolchain|^go ' third_party/widget/go.mod)
    else
        printf '    %s(not imported yet)%s\n' "$DIM" "$RST"
    fi
fi
