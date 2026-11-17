# Termust: shim de ZDOTDIR (.zshenv).
# Conserva el ZDOTDIR original (pasado por host-exec), sourcea el del usuario
# y vuelve al shim para que zsh lea .zprofile/.zshrc desde aquí.

export TERMUST_ORIGINAL_ZDOTDIR="${TERMUST_ORIGINAL_ZDOTDIR:-$HOME}"
export TERMUST_SHIM_ZDOTDIR="$ZDOTDIR"
export ZDOTDIR="$TERMUST_ORIGINAL_ZDOTDIR"
if [ -f "$ZDOTDIR/.zshenv" ]; then
    . "$ZDOTDIR/.zshenv"
fi
export ZDOTDIR="$TERMUST_SHIM_ZDOTDIR"
