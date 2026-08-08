# Workforest shell integration — print with `workforest shell-init` and eval
# from your shell rc:   eval "$(workforest shell-init)"
#
# The wf function runs workforest and evals a `cd ...` directive from its
# stdout (a subprocess cannot change this shell's directory); any other
# stdout is passed through untouched. Human messages arrive on stderr.

wf() {
    local out
    out="$(workforest "$@")" || return $?
    case "$out" in
        "cd "*) eval "$out" ;;
        "") ;;
        *) printf '%s\n' "$out" ;;
    esac
}
