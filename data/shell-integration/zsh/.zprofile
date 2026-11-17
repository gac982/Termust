# Termust: shim de ZDOTDIR (.zprofile).

export ZDOTDIR="$TERMUST_ORIGINAL_ZDOTDIR"
if [ -f "$ZDOTDIR/.zprofile" ]; then
    . "$ZDOTDIR/.zprofile"
fi
export ZDOTDIR="$TERMUST_SHIM_ZDOTDIR"
