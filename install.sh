#!/bin/sh
# openside installer
#   curl -fsSL https://openside.schnnjuan.com/install.sh | sh
# flags: OPENSIDE_LANG=en|pt · OPENSIDE_PREFIX=dir
set -eu

REPO="schnnjuan/openside"
PREFIX="${OPENSIDE_PREFIX:-$HOME/.local/bin}"
SHARE="$HOME/.local/share/openside"
ENGINE_REPO="https://github.com/$REPO"
ENGINE_DIR="$SHARE/engine/src/iPASide.Engine"

# ---- colors ----
E=$(printf '\033')
RESET="$E[0m"
GRN="$E[38;2;100;224;164m"
CY="$E[38;2;36;160;103m"
GRAY="$E[38;2;93;138;119m"
RED="$E[38;2;174;85;68m"
HIDE="$E[?25l"
SHOW="$E[?25h"

die() { printf '%s\n\n  %s%s%s\n\n' "$SHOW" "$RED" "$1" "$RESET" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || die "curl missing — install it and try again"
command -v awk  >/dev/null 2>&1 || die "awk missing — install it and try again"
command -v git  >/dev/null 2>&1 || die "git missing — install it and try again"
command -v python3 >/dev/null 2>&1 || die "python3 missing — openside needs python 3.12+"
python3 -c 'import sys; sys.exit(0 if sys.version_info >= (3,12) else 1)' \
    || die "python 3.12+ required (found $(python3 -c 'import sys; print("%d.%d" % sys.version_info[:2])'))"

# ---- tty: stdin is the pipe under curl|sh, so read choices from /dev/tty ----
HAS_TTY=0
OLD_STTY=""
if [ "${OPENSIDE_NO_TTY:-}" != 1 ] && [ -t 2 ] && [ -r /dev/tty ] && (exec 3</dev/tty) 2>/dev/null; then
    exec 3</dev/tty
    HAS_TTY=1
    OLD_STTY=$(stty -g </dev/tty 2>/dev/null || true)
fi
cleanup() {
    if [ "$HAS_TTY" = 1 ] && [ -n "$OLD_STTY" ]; then
        stty "$OLD_STTY" </dev/tty 2>/dev/null || true
    fi
    printf '%s' "$SHOW"
}
trap cleanup EXIT

# ---- header ----
printf '%s' "$HIDE"
printf '\n'
# logo: real PNG via kitty/ghostty/wezterm/iterm2 image protocols; ASCII fallback
LOGO_B64=$(cat <<'EOB' | tr -d '\n'
iVBORw0KGgoAAAANSUhEUgAAApQAAACQCAYAAAC2wfsDAAAExElEQVR4nO3dIY9UZxiG4ZlmBApX
hQHLH6huqqGiCkKCIVWtKAqDwqBAFNVgSAgoDGiCJvW1ranCkZlApmL5B5O8efrteT/OdendzXO+
Obt755iz3QDACtz55/nZyJ//9PKt7cifvzY+r7l8s/QAAADmJigBAIgISgAAIoISAICIoAQAICIo
AQCICEoAACKCEgCAiKAEACAiKAEAiAhKAAAiu6UHAMB52O8PS0+gwOc1F08oAQCICEoAACKCEgCA
iKAEACAiKAEAiAhKAAAighIAgIigBAAgIigBAIgISgAAIoISAIDItvoN194/Ohsx5Ly8+e5u+ZpH
cp7/L+d52uznU+X+XFa38+e02e/Pbvfb2s7TE0oAACKCEgCAiKAEACAiKAEAiAhKAAAighIAgIig
BAAgIigBAIgISgAAIoISAICIoAQAILL9/u2Doe+afPfD/aHv1uy2v9ueqm77u+2p6ra/256qbvu7
7eG0tX1es19vt/3d9lSN3u8JJQAAEUEJAEBEUAIAEBGUAABEBCUAABFBCQBARFACABARlAAARAQl
AAARQQkAQERQAgAQ2e33h6U3RLrt77anqtv+bnuquu3vtqeq2/5uezhtbZ/X7NfbbX+3PVWj93tC
CQBARFACABARlAAARAQlAAARQQkAQERQAgAQEZQAAEQEJQAAEUEJAEBEUAIAEBGUAABEdofD3O+m
7La/256qbvtH77n64tezkT9/bec5Wrf9f/74cFv5+ur9Nvr+rPrr5u+l6+2m2/0z2uzX222//0en
eUIJAEBEUAIAEBGUAABEBCUAABFBCQBARFACABARlAAARAQlAAARQQkAQERQAgAQEZQAAER2n/af
lt4Q6ba/+q7bK3/cKb27s/r1VdXz/Pvnp0Pf7Tv68x29v5tuvy9Vs+/v9i7s0X9/uv1+zX7/VM1+
vd32+390mieUAABEBCUAABFBCQBARFACABARlAAARAQlAAARQQkAQERQAgAQEZQAAEQEJQAAEUEJ
AEBkdzz8t/SGSLf9lx7fLr3rtrr/39+eTf2uz6pun+/sZj/P2fd3s7bzdL1z6ba/255uPKEEACAi
KAEAiAhKAAAighIAgIigBAAgIigBAIgISgAAIoISAICIoAQAICIoAQCICEoAACK74/649IZIt/3d
9sxu9vP89sGN0rvdqz7cf1l6t/vs5zn7/m7Wdp6udy7d9nfbUzX6/5EnlAAARAQlAAARQQkAQERQ
AgAQEZQAAEQEJQAAEUEJAEBEUAIAEBGUAABEBCUAABFBCQBApPQe4M1ms7l476eh74Ic7ePDV+Vr
rhh9PqP3z879edrs92e3/bPfb1Wz//3pdv90M/v93O3813aenlACABARlAAARAQlAAARQQkAQERQ
AgAQEZQAAEQEJQAAEUEJAEBEUAIAEBGUAABEBCUAAJFW7738Glz45frQd3d+fvLaZwYAtOIJJQAA
EUEJAEBEUAIAEBGUAABEBCUAABFBCQBARFACABARlAAARAQlAAARQQkAQERQAgAQ2S094GtzPByX
ngAAcK48oQQAICIoAQCICEoAACKCEgCAiKAEACAiKAEAiAhKAAAighIAgIigBAAgIigBAIgISgAA
Il8AYsRgbmxWyEEAAAAASUVORK5CYII=
EOB
)
IMG=ascii
case "${TERM_PROGRAM:-}" in WezTerm*|iTerm.app) IMG=iterm ;; esac
if [ "$IMG" = ascii ]; then
    case "${TERM:-}" in *kitty*|*ghostty*) IMG=kitty ;; esac
fi
if [ "$IMG" = ascii ]; then
    [ -n "${KITTY_WINDOW_ID:-}" ] && IMG=kitty
    [ -n "${GHOSTTY_RESOURCES_DIR:-}" ] && IMG=kitty
fi
[ -n "${TMUX:-}" ] && IMG=ascii
if [ "$IMG" = kitty ]; then
    printf '%s' "$LOGO_B64" | fold -w 4000 | awk -v E="$E" '
        { c[NR]=$0 }
        END {
            for (i=1;i<=NR;i++) {
                if (i==1) printf "%s_Gf=100,a=T,q=1,c=55,m=%d;%s%s\\", E, (i==NR?0:1), c[i], E
                else      printf "%s_Gm=%d;%s%s\\", E, (i==NR?0:1), c[i], E
            }
            printf "\n"
        }'
elif [ "$IMG" = iterm ]; then
    printf '%s]1337;File=inline=1;width=55;size=%d:%s\a\n' "$E" $(( ${#LOGO_B64} * 3 / 4 )) "$LOGO_B64"
else
    # ASCII fallback: same glyphs the tui draws, svg gradient, full blocks only
    python3 - "$E" <<'PYEOF' 2>/dev/null || true
import sys
E = sys.argv[1] if len(sys.argv) > 1 else "\033"
G = {
 "o":["......","......",".####.","##..##","##..##","##..##","##..##","##..##","##..##",".####.","......","......"],
 "p":["......","......",".####.","##..##","##..##","##..##",".####.","##....","##....","##....","##....","##...."],
 "e":["......","......",".####.","##..##","##..##","######","##....","##....","##..##",".####.","......","......"],
 "n":["......","......","#####.","##..##","##..##","##..##","##..##","##..##","##..##","##..##","......","......"],
 "s":["......","......",".#####","##....","##....",".####.","....##","....##","....##","#####.","......","......"],
 "i":["..##..","..##..","......","..##..","..##..","..##..","..##..","..##..","..##..","..##..","......","......"],
 "d":["....##","....##",".####.","##..##","##..##","##..##","##..##","##..##","##..##",".####.","......","......"],
}
STOPS=[(100,224,164),(36,160,103),(2,97,72)]
def grad(t):
    n=len(STOPS)-1;seg=min(int(t*n),n-1);f=t*n-seg
    a,b=STOPS[seg],STOPS[seg+1]
    return tuple(round(x+(y-x)*f) for x,y in zip(a,b))
word="openside"; SLOT=12
W=len(word)*6+len(word)-1
grid=[[0]*W for _ in range(SLOT)]
x=0
for c in word:
    for ry,row in enumerate(G[c]):
        for cx,ch in enumerate(row):
            if ch=="#": grid[ry][x+cx]=1
    x+=7
for y in range(SLOT):
    c=grad(y/(SLOT-1))
    line="".join((f"{E}[38;2;{c[0]};{c[1]};{c[2]}m█" if p else " ") for p in grid[y])
    print(f"{E}[39;49m"+line+f"{E}[39;49m")
PYEOF
fi

printf '\n'
printf '  %ssideload .ipas from a beautiful terminal%s\n\n' "$GRAY" "$RESET"

# ---- language ----
LANG_CHOICE="${OPENSIDE_LANG:-}"
if [ -z "$LANG_CHOICE" ] && [ "$HAS_TTY" = 1 ]; then
    printf '  %slanguage / idioma%s\n\n' "$CY" "$RESET"
    stty -icanon -echo </dev/tty 2>/dev/null || true
    sel=1
    ESC=$(printf '\033')
    CR=$(printf '\r')
    draw_lang() {
        printf '%s' "${E}[2A${E}[J"
        if [ "$sel" = 1 ]; then
            printf '  %s❯ english%s\n  %s  português%s\n' "$GRN" "$RESET" "$GRAY" "$RESET"
        else
            printf '  %s  english%s\n  %s❯ português%s\n' "$GRAY" "$RESET" "$GRN" "$RESET"
        fi
    }
    printf '  \n\n'
    draw_lang
    while :; do
        k=$(dd bs=1 count=1 <&3 2>/dev/null || true)
        if [ "$k" = "$ESC" ]; then
            rest=$(dd bs=1 count=2 <&3 2>/dev/null || true)
            case "$rest" in
                '[A'|'OA') sel=1 ;;
                '[B'|'OB') sel=2 ;;
                *) break ;; # esc alone: accept current
            esac
            draw_lang
        elif [ "$k" = "$CR" ] || [ -z "$k" ]; then
            break
        elif [ "$k" = "k" ] || [ "$k" = "1" ]; then
            sel=1; draw_lang
        elif [ "$k" = "j" ] || [ "$k" = "2" ]; then
            sel=2; draw_lang
        fi
    done
    stty "$OLD_STTY" </dev/tty 2>/dev/null || true
    [ "$sel" = 1 ] && LANG_CHOICE=en || LANG_CHOICE=pt
    printf '\n'
elif [ -z "$LANG_CHOICE" ]; then
    LANG_CHOICE=en
    printf '  %slanguage / idioma%s  english  %s(OPENSIDE_LANG=pt for português)%s\n\n' "$CY" "$RESET" "$GRAY" "$RESET"
else
    printf '  %slanguage / idioma%s  %s\n\n' "$CY" "$RESET" "$LANG_CHOICE"
fi

# ---- copy ----
if [ "$LANG_CHOICE" = pt ]; then
    M_CLONE="buscando o motor (clone raso)"
    M_PY_DEPS="instalando o motor de assinatura"
    M_VENV_MAKE="criando o venv do motor"
    M_CARGO="compilando openside (primeira vez demora alguns minutos)"
    M_RUST="rust não encontrado — instalando via rustup"
    M_WRAP="criando o comando openside em"
    M_DONE="✔ instalado em"
    M_RUN="rode:"
    M_PATH="$PREFIX não está no PATH — adiciona ao teu shell profile"
    M_CARGO_MISS="cargo missing — run sh <(curl -fsSL https://sh.rustup.rs) and rerun"
    M_VENV="não consegui criar o venv (python3-venv instalado?)"
    M_PIP="falha ao instalar o motor — confere a conexão e tenta de novo"
    M_BUILD="falha ao compilar o openside — tenta rodar cargo install openside na mão e me manda o erro"
else
    M_CLONE="fetching the engine (shallow clone)"
    M_PY_DEPS="installing the signing engine"
    M_VENV_MAKE="creating the engine venv"
    M_CARGO="compiling openside (first build takes a few minutes)"
    M_RUST="rust not found — installing via rustup"
    M_WRAP="creating the openside command in"
    M_DONE="✔ installed"
    M_RUN="run:"
    M_PATH="$PREFIX is not in your PATH — add it to your shell profile"
    M_CARGO_MISS="cargo missing — install manually: sh <(curl -fsSL https://sh.rustup.rs) then rerun me"
    M_VENV="could not create the venv (python3-venv installed?)"
    M_PIP="engine install failed — check your connection and try again"
    M_BUILD="openside build failed — try cargo install openside by hand and send me the error"
fi

spinner() { # $1 = background pid
    i=0
    while kill -0 "$1" 2>/dev/null; do
        case $((i % 4)) in
            0) S='▘' ;; 1) S='▝' ;; 2) S='▗' ;; 3) S='▖' ;;
        esac
        printf '\r  %s%s%s  ' "$CY" "$S" "$RESET"
        i=$((i + 1))
        sleep 0.08
    done
}
endspin() { printf '\r  \033[K\n'; }

# ---- rustup bootstrap (before HOME-known msgs) ----
if ! command -v cargo >/dev/null 2>&1; then
    printf '  %s%s%s\n' "$GRAY" "$M_RUST" "$RESET"
    if ! curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal; then
        die "$M_CARGO_MISS"
    fi
    . "$HOME/.cargo/env" 2>/dev/null || . "$HOME/.profile" 2>/dev/null || true
fi
# shim do rustup instalado mas sem toolchain? escolhe o stable na hora
if ! cargo --version >/dev/null 2>&1 && command -v rustup >/dev/null 2>&1; then
    rustup -y default stable >/dev/null 2>&1 || true
fi
command -v cargo >/dev/null 2>&1 && cargo --version >/dev/null 2>&1 || die "$M_CARGO_MISS"

# ---- engine: shallow clone + venv + requirements ----
mkdir -p "$SHARE"
printf '  %s%s%s\n' "$GRAY" "$M_CLONE" "$RESET"
[ -d "$ENGINE_DIR/ipaside_engine" ] || {
    ( git clone -q --depth 1 "$ENGINE_REPO" "$SHARE/engine" ) &
    PID=$!
    spinner "$PID"
    endspin
    wait "$PID" || die "$M_PIP"
}
printf '  %s%s%s\n' "$GRAY" "$M_VENV_MAKE" "$RESET"
( python3 -m venv "$SHARE/venv" ) &
PID=$!
spinner "$PID"
endspin
wait "$PID" || die "$M_VENV"
printf '  %s%s%s\n' "$GRAY" "$M_PY_DEPS" "$RESET"
( "$SHARE/venv/bin/pip" install -q --disable-pip-version-check -r "$ENGINE_DIR/requirements.txt" ) &
PID=$!
spinner "$PID"
endspin
wait "$PID" || die "$M_PIP"

# ---- openside: cargo install from crates.io ----
printf '  %s%s%s\n' "$GRAY" "$M_CARGO" "$RESET"
( cargo install -q openside ) &
PID=$!
spinner "$PID"
endspin
wait "$PID" || die "$M_BUILD"
BIN=$(command -v openside 2>/dev/null || true)
[ -x "$BIN" ] || BIN="$HOME/.cargo/bin/openside"
[ -x "$BIN" ] || die "$M_BUILD"

# ---- launcher: points the tui at the engine venv ----
printf '  %s%s %s%s\n' "$GRAY" "$M_WRAP" "$PREFIX" "$RESET"
mkdir -p "$PREFIX"
if [ -f "$PREFIX/openside" ] && head -2 "$PREFIX/openside" | grep -q "install.sh"; then
    : # ours from a previous run, safe to replace
elif [ -f "$PREFIX/openside" ]; then
    die "$PREFIX/openside already exists and is not mine — remove it and rerun"
fi
cat > "$PREFIX/openside" <<WEOF
#!/bin/sh
# openside launcher (install.sh) — wires the tui to the engine venv
exec env OPENSIDE_ENGINE_PYTHON="$SHARE/venv/bin/python" OPENSIDE_ENGINE_DIR="$ENGINE_DIR" "$BIN" "\$@"
WEOF
chmod 755 "$PREFIX/openside"

# ---- done ----
printf '  %s%s %s%s\n' "$GRN" "$M_DONE" "$PREFIX/openside" "$RESET"
printf '  %s%s%s  openside\n' "$GRAY" "$M_RUN" "$RESET"
case ":$PATH:" in
    *":$PREFIX:"*) ;;
    *) printf '\n  %s%s%s\n' "$GRAY" "$M_PATH" "$RESET" ;;
esac
printf '\n'
