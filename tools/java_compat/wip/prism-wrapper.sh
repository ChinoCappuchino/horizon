#!/usr/bin/env bash
# Prism Launcher wrapper (instance "Horizon Fork - Fresh Test"): Prism downloads
# Minecraft 26.3 and runs this in place of Java. It plays the fork's build with
# the mods in this instance's own mods folder, as any modded instance does: the
# build installed as a launcher version, the resources prepared from Prism's own
# Minecraft, the instance's jars imported and ported, then the game (with the
# startup splash).
#
# Launches are cached: one launcher folder, the build installed and prepared
# once per build, the mods ported again only when the build or the mods folder
# changed; worlds and options stay. HORIZON_FRESH_CLEAN=1: a first fresh
# download instead (a new folder, every step).
set -euo pipefail
FORK=/mnt/shared/Horizon/horizon-fork
BUILD=/mnt/shared/Horizon/target/release
PRISM="$HOME/.local/share/PrismLauncher"
# The instance Prism launches (its folder holds the jars), or the one
# HORIZON_FRESH_PACK names.
PACK_DIR="${HORIZON_FRESH_PACK:-${INST_DIR:-$PRISM/instances/Horizon Fork - Fresh Test}}"
PACK_MODS="$PACK_DIR/minecraft/mods"

# The whole setup under the startup splash: the client runs this script again
# as its child and shows each step's progress; the game's own splash takes
# over from it. (HORIZON_FRESH_NO_SPLASH=1: without.)
JAR="${HORIZON_FRESH_MINECRAFT:-$PRISM}/libraries/com/mojang/minecraft/26.3/minecraft-26.3-client.jar"
if [ -z "${HORIZON_SETUP_SPLASH:-}" ] && [ -z "${HORIZON_FRESH_NO_SPLASH:-}" ] \
    && [ -x "$BUILD/horizon-client" ] && [ -f "$JAR" ]; then
    export HORIZON_SETUP_SPLASH=1
    exec "$BUILD/horizon-client" --setup-splash --jar "$JAR" -- "$0" "$@"
fi

# The player's account, from Prism (its signed-in Microsoft account); never
# printed. Prism runs its own Java launcher through this wrapper and pipes the
# game's parameters to it on stdin ("param <value>" lines, then "launch"):
# they are read from there, and from the arguments too.
NAME=Player
UUID=""
TOKEN=""
XUID=""
args=("$@")
if [ ! -t 0 ]; then
    while IFS= read -r line; do
        case "$line" in
            "param "*) args+=("${line#param }") ;;
            launch | abort) break ;;
        esac
    done
fi
for ((i = 0; i + 1 < ${#args[@]}; i++)); do
    case "${args[$i]}" in
        --username) NAME="${args[$((i + 1))]}" ;;
        --uuid) UUID="${args[$((i + 1))]}" ;;
        --accessToken) TOKEN="${args[$((i + 1))]}" ;;
        --xuid) XUID="${args[$((i + 1))]}" ;;
    esac
done

# The runs on a local disk: a player's install is, and /mnt/shared (virtiofs)
# made preparing ten times slower (979 s against 102 s).
RUNS="${HORIZON_FRESH_RUNS:-$HOME/.cache/horizon-fresh-test}"
if [ -n "${HORIZON_FRESH_CLEAN:-}" ]; then
    RUN="$RUNS/run-$(date +%Y%m%d-%H%M%S)"
    # Keep the two newest fresh runs.
    ls -dt "$RUNS"/run-* 2>/dev/null | tail -n +2 | while read -r old; do
        case "$old" in "$RUNS"/run-*) rm -rf -- "$old" ;; esac
    done
else
    RUN="$RUNS/cached"
fi
mkdir -p "$RUN"
export HORIZON_LAUNCHER_HOME="$RUN/launcher"
export HORIZON_HOME="$RUN/home"
export HORIZON_LAUNCHER_NO_SELF_UPDATE=1
L="$BUILD/horizon-launcher"
TAG="fork-$(git -C "$FORK" rev-parse --short HEAD)"
VERSION="$HORIZON_LAUNCHER_HOME/versions/$TAG"
step() { printf '\n== %s ==\n' "$*"; }

echo "launcher folder $RUN"
step "1/5 Installing the fork build as $TAG"
if [ -f "$VERSION/.installed" ]; then
    echo "installed already"
else
    "$BUILD/xtask" package "$BUILD/horizon-client" "$VERSION"
    touch "$VERSION/.installed"
fi

step "2/5 Preparing the game's resources from Minecraft 26.3"
if [ -f "$VERSION/.prepared" ]; then
    echo "prepared already"
else
    # (HORIZON_FRESH_MINECRAFT points at other game files, for testing.)
    "$L" prepare --minecraft "${HORIZON_FRESH_MINECRAFT:-$PRISM}" --version "$TAG"
    touch "$VERSION/.prepared"
fi

step "3/5 Signing in as $NAME"
if [ -n "$TOKEN" ] && [ "$TOKEN" != "0" ]; then
    # The game reads this once and deletes it (horizon-client --session-file).
    SESSION="$RUN/session.json"
    (umask 077; printf '{"name":"%s","uuid":"%s","access_token":"%s","user_type":"msa","xuid":"%s"}' \
        "$NAME" "$UUID" "$TOKEN" "$XUID" > "$SESSION")
    export HORIZON_SESSION_FILE="$SESSION"
    echo "signed in"
else
    echo "no Microsoft session from Prism: sign in to an account that owns Minecraft there"
fi
unset TOKEN

step "4/5 Porting the modpack's mods"
[ -d "$PACK_MODS" ] && ls "$PACK_MODS"/*.jar >/dev/null 2>&1 \
    || { echo "no mods in $PACK_MODS: put the mods' jars there"; exit 1; }
# What was ported: the build and every jar (name, size, time).
FINGERPRINT=$( { echo "$TAG"; ls -l --time-style=+%s "$PACK_MODS"; } | sha256sum | cut -d' ' -f1)
STAMP="$HORIZON_HOME/.ported"
PACK=""
if [ -f "$STAMP" ] && [ "$(head -1 "$STAMP")" = "$FINGERPRINT" ]; then
    PACK=$(sed -n 2p "$STAMP")
    echo "ported already ($PACK): the build and the mods folder are unchanged"
fi
if [ -z "$PACK" ]; then
    "$L" modpack import "$PACK_DIR"
    PACK=$("$L" --json modpack list | python3 -I -c 'import re,sys; m=re.search(r"\"pack\":\s*\"([^\"]+)\"", sys.stdin.read()); print(m.group(1) if m else "")')
    [ -n "$PACK" ] || { echo "the pack did not import"; exit 1; }
    # Mods ported before but gone from the folder go too (worlds stay).
    rm -rf -- "$HORIZON_HOME/ports/$PACK/mods"
    "$L" modpack port "$PACK" --from "$PACK_MODS" --version "$TAG"
    printf '%s\n%s\n' "$FINGERPRINT" "$PACK" > "$STAMP"
fi

# (HORIZON_FRESH_NO_PLAY=1 stops here, for testing the steps.)
[ -z "${HORIZON_FRESH_NO_PLAY:-}" ] || exit 0
step "5/5 Starting the game"
exec "$L" modpack play "$PACK" --version "$TAG"
