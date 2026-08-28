# Workforest shell integration — print with `workforest shell-init` and eval
# from your shell rc:   eval "$(workforest shell-init)"
#
# Both `workforest` and `wf` become the same shell function: it runs the
# workforest binary and evals a directive from its stdout, marked by a
# leading US control byte (0x1f) — a subprocess cannot change this shell's
# directory, and the marker keeps data output (listings, dumps) from ever
# being mistaken for something to execute. Any other stdout is passed
# through untouched. Human messages arrive on stderr. Defining both names
# means an alias for either spelling (`alias wfo='workforest open'`)
# behaves the same.

workforest() {
    local out directive=$'\x1f'
    out="$(command workforest "$@")" || return $?
    case "$out" in
        "$directive"*) eval "${out#"$directive"}" ;;
        "") ;;
        *) printf '%s\n' "$out" ;;
    esac
}

wf() {
    workforest "$@"
}
