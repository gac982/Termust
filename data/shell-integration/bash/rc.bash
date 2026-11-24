# Integración de Termust para bash (rc efímero).
# No modifica los dotfiles del usuario: se pasa con --rcfile.

if [ -f "$HOME/.bashrc" ]; then
    . "$HOME/.bashrc"
fi

__termust_osc7() {
    printf '\033]7;file://%s%s\007' "${HOSTNAME:-localhost}" "$PWD"
}

__termust_marker=$'\033]133;B\007'

# PS0 se expande después de leer un comando y antes de ejecutarlo. En ese
# momento `history 1` ya contiene el comando actual, incluso si después se
# interrumpe con Ctrl+C.
__termust_ps0() {
    local line cmd
    line=$(HISTTIMEFORMAT= history 1 2>/dev/null)
    line=${line#"${line%%[![:space:]]*}"}
    cmd=${line#*[[:space:]]}
    cmd=${cmd#"${cmd%%[![:space:]]*}"}
    if [ -n "$cmd" ]; then
        printf '\033]633;E;%s\007\033]133;C\007' "$cmd"
    fi
}

__termust_precmd() {
    local ec=$?
    printf '\033]133;D;%s\007' "$ec"
    __termust_osc7
    printf '\033]133;A\007'
    case "$PS1" in
        *"$__termust_marker"*) ;;
        *) PS1="${PS1}\[${__termust_marker}\]" ;;
    esac
    return "$ec"
}

case "${PROMPT_COMMAND-}" in
    *__termust_precmd*) ;;
    *) PROMPT_COMMAND="__termust_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}" ;;
esac

PS0="${PS0-}"'$(__termust_ps0)'
printf '\033]133;A\007'
