#!/bin/sh
# Installs the Trailhead window's desktop entry and icons under ~/.local/share, the entry running the given absolute
# path with the given arguments: the dock's PATH often lacks ~/.cargo/bin. The entry's name must match the window's
# app id, `trailhead`, for the dock to show its icon. `mise run shell-install` and `shell-install-standalone` run it.
set -eu

if [ $# -lt 1 ]; then
    echo "usage: $0 <absolute path> [argument...]" >&2
    exit 2
fi
bin=$1
shift
case $bin in
    /*) ;;
    *) echo "$0: $bin is not an absolute path" >&2; exit 2 ;;
esac
# Characters a desktop entry's Exec would need escaped; a path with them is refused rather than written wrong.
case $bin in
    *[\"\`\$\\%]*) echo "$0: $bin has a character the desktop entry can't hold as it is" >&2; exit 2 ;;
esac
if [ ! -x "$bin" ]; then
    echo "$0: $bin isn't installed" >&2
    exit 1
fi

assets=$(dirname "$0")/assets
share=$HOME/.local/share
exec_line="Exec=\"$bin\""
for arg in "$@"; do
    exec_line="$exec_line $arg"
done

mkdir -p "$share/applications"
entry=$share/applications/trailhead.desktop
while IFS= read -r line; do
    if [ "$line" = "Exec=trailhead app" ]; then
        printf '%s\n' "$exec_line"
    else
        printf '%s\n' "$line"
    fi
done < "$assets/trailhead.desktop" > "$entry"
if ! grep -qxF "$exec_line" "$entry"; then
    echo "trailhead.desktop: Exec line not rewritten (the asset must have exactly \"Exec=trailhead app\")" >&2
    exit 1
fi
chmod 644 "$entry"
for size in 32 64 128 256; do
    install -Dm644 "$assets/icon/$size.png" "$share/icons/hicolor/${size}x${size}/apps/trailhead.png"
done
install -Dm644 "$assets/icon/trailhead.svg" "$share/icons/hicolor/scalable/apps/trailhead.svg"
update-desktop-database "$share/applications" 2>/dev/null || true
gtk-update-icon-cache -q -t "$share/icons/hicolor" 2>/dev/null || true
echo "installed $entry: $exec_line"
