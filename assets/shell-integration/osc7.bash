__gototerm_osc7() {
    printf '\033]7;file://%s%s\033\\' "$HOSTNAME" "$PWD"
}

case ";${PROMPT_COMMAND:-};" in
    *";__gototerm_osc7;"*) ;;
    *) PROMPT_COMMAND="__gototerm_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
esac
