# bash completion for workforest / wf.
# Dynamic candidates are delegated to `workforest --complete TOPIC`.

_workforest_complete() {
    local cur prev cmd topic
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"
    cmd="${COMP_WORDS[1]:-}"
    if [ "$COMP_CWORD" -eq 1 ]; then
        topic=commands
    elif [ "$prev" = -o ] || [ "$prev" = --opener ]; then
        topic=openers
    else
        case "$cmd" in
            create) topic=branches ;;
            open|delete|checkout) topic=worktrees ;;
            run|stop) topic=scripts ;;
            claude) topic=claude-sessions ;;
            tui|list|init|config|shell-init) topic=none ;;
            *) topic=worktrees ;;
        esac
    fi
    COMPREPLY=()
    if [ "$topic" != none ]; then
        local IFS=$'\n'
        # cut: some topics emit NAME<TAB>DESCRIPTION; bash cannot display
        # descriptions, so keep only the name field.
        COMPREPLY=($(compgen -W "$(command workforest --complete "$topic" 2>/dev/null | cut -f1)" -- "$cur"))
    fi
}

complete -F _workforest_complete workforest wf
