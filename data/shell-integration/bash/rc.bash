# Integración de Termust para bash (rc efímero).
# No modifica los dotfiles del usuario: se pasa con --rcfile.

if [ -f "$HOME/.bashrc" ]; then
    . "$HOME/.bashrc"
fi

__termust_osc7() {
    printf '\033]7;file://%s%s\007' "${HOSTNAME:-localhost}" "$PWD"
}

__termust_precmd() {
    local ec=$?
    printf '\033]133;D;%s\007' "$ec"
    __termust_osc7
    printf '\033]133;A\007'
}

__termust_preexec() {
    printf '\033]133;B\007\033]133;C\007'
}

case "${PROMPT_COMMAND-}" in
    *__termust_precmd*) ;;
    *) PROMPT_COMMAND="__termust_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
esac

trap '__termust_preexec' DEBUG
printf '\033]133;A\007'
