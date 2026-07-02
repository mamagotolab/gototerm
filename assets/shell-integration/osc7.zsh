__gototerm_osc7() {
    printf '\033]7;file://%s%s\033\\' "$HOST" "$PWD"
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd __gototerm_osc7
