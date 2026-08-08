# zsh completion for workforest / wf (sourced by `workforest shell-init`).
# Dynamic candidates are delegated to `workforest --complete TOPIC`.

_workforest_complete() {
    local topic cmd
    local -a items
    cmd="${words[2]:-}"
    if (( CURRENT == 2 )); then
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
    if [[ "$topic" != none ]]; then
        items=(${(f)"$(workforest --complete "$topic" 2>/dev/null)"})
        (( ${#items} )) && compadd -a items
    fi
}

if (( ${+functions[compdef]} )); then
    compdef _workforest_complete workforest wf
else
    # compinit has not run yet (shell-init was eval'ed before it in the rc).
    # Defer registration to just before the first prompt, when compinit is
    # done; the hook removes itself after one shot.
    _workforest_register_completion() {
        add-zsh-hook -d precmd _workforest_register_completion
        (( ${+functions[compdef]} )) && compdef _workforest_complete workforest wf
    }
    autoload -Uz add-zsh-hook
    add-zsh-hook precmd _workforest_register_completion
fi
