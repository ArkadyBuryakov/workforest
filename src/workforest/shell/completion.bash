# bash completion for workforest / wf.
# Dynamic candidates are delegated to `workforest --complete TOPIC`.

_workforest_complete() {
    local cur cmd topic
    cur="${COMP_WORDS[COMP_CWORD]}"
    cmd="${COMP_WORDS[1]:-}"
    if [ "$COMP_CWORD" -eq 1 ]; then
        topic=commands
    else
        case "$cmd" in
            create) topic=branches ;;
            open|delete|checkout) topic=worktrees ;;
            run) topic=scripts ;;
            claude) topic=claude-sessions ;;
            tui|list|init|config|shell-init) topic=none ;;
            *) topic=worktrees ;;
        esac
    fi
    COMPREPLY=()
    if [ "$topic" != none ]; then
        local IFS=$'\n'
        COMPREPLY=($(compgen -W "$(workforest --complete "$topic" 2>/dev/null)" -- "$cur"))
    fi
}

complete -F _workforest_complete workforest wf
