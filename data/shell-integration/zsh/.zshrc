# Termust: shim de ZDOTDIR (.zshrc).
# Sourcea el rc del usuario con su ZDOTDIR, instala hooks y restaura ZDOTDIR.

export ZDOTDIR="$TERMUST_ORIGINAL_ZDOTDIR"
if [ -f "$ZDOTDIR/.zshrc" ]; then
    . "$ZDOTDIR/.zshrc"
fi
export ZDOTDIR="$TERMUST_SHIM_ZDOTDIR"

autoload -Uz add-zsh-hook

__termust_osc7() {
    printf '\033]7;file://%s%s\007' "${HOST:-localhost}" "$PWD"
}

__termust_prompt_marker=$'\033]133;B\007'

__termust_precmd() {
    local ec=$?
    printf '\033]133;D;%s\007' "$ec"
    __termust_osc7
    printf '\033]133;A\007'
    printf '\033]633;V;%s\007' "${KEYMAP:-main}"
    case "${PROMPT-}" in
        *"$__termust_prompt_marker"*) ;;
        *) PROMPT="${PROMPT-}%{${__termust_prompt_marker}%}" ;;
    esac
}

__termust_preexec() {
    printf '\033]633;E;%s\007' "$1"
    printf '\033]133;C\007'
}

__termust_keymap() {
    printf '\033]633;V;%s\007' "${KEYMAP:-main}"
}

autoload -Uz add-zle-hook-widget 2>/dev/null
if (( $+functions[add-zle-hook-widget] )); then
    add-zle-hook-widget keymap-select __termust_keymap
else
    zle -N zle-keymap-select __termust_keymap
fi

add-zsh-hook precmd __termust_precmd
add-zsh-hook preexec __termust_preexec
printf '\033]133;A\007'

export ZDOTDIR="$TERMUST_ORIGINAL_ZDOTDIR"
