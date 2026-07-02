function __gototerm_osc7 --on-event fish_prompt
    printf '\033]7;file://%s%s\033\\' (hostname) "$PWD"
end
