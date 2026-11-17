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

__termust_precmd() {
    local ec=$?
    printf '\033]133;D;%s\007' "$ec"
    __termust_osc7
    printf '\033]133;A\007'
}

__termust_preexec() {
    printf '\033]133;B\007\033]133;C\007'
}

add-zsh-hook precmd __termust_precmd
add-zsh-hook preexec __termust_preexec
printf '\033]133;A\007'

export ZDOTDIR="$TERMUST_ORIGINAL_ZDOTDIR"
