#!/usr/bin/env bash
# Build and install the nerd-deck host app for the current user.
#
#   ./install.sh               build, install binary, udev rule, service
#   ./install.sh --no-udev     skip the udev rule (no sudo needed)
#   ./install.sh --no-firmware don't bundle the device firmware (no PlatformIO needed)
#   ./install.sh --no-service  don't install/enable the systemd user service
#   ./install.sh --no-tray     don't start the tray icon at login
#   ./install.sh --uninstall   remove everything this script installed (config is kept)

set -euo pipefail

HOST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="$HOME/.local/bin"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/nerd-deck"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
AUTOSTART_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
APPS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICON_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"
UDEV_RULE=/etc/udev/rules.d/99-nerd-deck.rules
SERVICE=nerd-deck.service
TRAY=nerd-deck-tray.desktop

do_udev=1
do_service=1
do_tray=1
do_firmware=1
uninstall=0

for arg in "$@"; do
    case "$arg" in
        --no-udev) do_udev=0 ;;
        --no-service) do_service=0 ;;
        --no-tray) do_tray=0 ;;
        --no-firmware) do_firmware=0 ;;
        --uninstall) uninstall=1 ;;
        -h | --help)
            sed -n '2,10s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"
            exit 0
            ;;
        *)
            echo "unknown option: $arg (see --help)" >&2
            exit 1
            ;;
    esac
done

info() { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
die() {
    printf '\033[1;31merror:\033[0m %s\n' "$*" >&2
    exit 1
}

uninstall() {
    if systemctl --user list-unit-files "$SERVICE" &>/dev/null; then
        info "Stopping and disabling $SERVICE"
        systemctl --user disable --now "$SERVICE" 2>/dev/null || true
    fi
    rm -f "$UNIT_DIR/$SERVICE"
    systemctl --user daemon-reload 2>/dev/null || true

    info "Removing the tray icon"
    pkill -u "$USER" -f "^[^ ]*nerd-deck --tray" || true
    rm -f "$AUTOSTART_DIR/$TRAY"

    info "Removing $BIN_DIR/nerd-deck and its menu entry"
    rm -f "$BIN_DIR/nerd-deck" "$APPS_DIR/nerd-deck.desktop" "$ICON_DIR/nerd-deck.svg"

    if [[ -e $UDEV_RULE ]]; then
        info "Removing $UDEV_RULE (needs sudo)"
        sudo rm -f "$UDEV_RULE"
        sudo udevadm control --reload
    fi

    info "Done. Config left in $CONFIG_DIR"
}

check_deps() {
    command -v cargo >/dev/null ||
        die "cargo not found. Install Rust: https://rustup.rs (then: source ~/.cargo/env)"
    command -v pkg-config >/dev/null && pkg-config --exists libudev fontconfig ||
        die "libudev or fontconfig headers not found. Install them: sudo apt install build-essential pkg-config libudev-dev libfontconfig1-dev"
}

# Builds the device firmware so the host bundles it and keeps the device up to
# date (host/build.rs). Skipped, with a warning, when PlatformIO isn't installed.
build_firmware() {
    local pio
    pio=$(command -v pio || echo "$HOME/.platformio/penv/bin/pio")
    if [[ ! -x $pio ]]; then
        warn "PlatformIO not found; building without device firmware, so the device won't be updated"
        return
    fi
    info "Building device firmware"
    "$pio" run --silent -d "$HOST_DIR/../firmware"
    export NERD_DECK_FIRMWARE="$HOST_DIR/../firmware/.pio/build/wt32-sc01-plus/firmware-merged.bin"
}

install_binary() {
    info "Building release binary"
    cargo build --release --locked --manifest-path "$HOST_DIR/Cargo.toml"

    info "Installing $BIN_DIR/nerd-deck"
    install -Dm755 "$HOST_DIR/target/release/nerd-deck" "$BIN_DIR/nerd-deck"

    info "Adding nerd-deck settings to the app menu"
    install -Dm644 "$HOST_DIR/packaging/nerd-deck.svg" "$ICON_DIR/nerd-deck.svg"
    mkdir -p "$APPS_DIR"
    sed "s|^Exec=.*|Exec=$BIN_DIR/nerd-deck --settings|" "$HOST_DIR/packaging/nerd-deck.desktop" >"$APPS_DIR/nerd-deck.desktop"
    refresh_icons
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) warn "$BIN_DIR is not on your PATH; the service doesn't need it, but your shell does" ;;
    esac
}

# Desktops cache icons and menu entries; tell them about the new ones.
refresh_icons() {
    touch "${ICON_DIR%/scalable/apps}"
    if command -v gtk-update-icon-cache >/dev/null && [[ -f ${ICON_DIR%/scalable/apps}/index.theme ]]; then
        gtk-update-icon-cache -q -t "${ICON_DIR%/scalable/apps}" || true
    fi
    command -v update-desktop-database >/dev/null && update-desktop-database -q "$APPS_DIR" || true
    command -v kbuildsycoca6 >/dev/null && kbuildsycoca6 >/dev/null 2>&1 || true
    # KDE's running apps keep their own icon cache until told it changed.
    command -v dbus-send >/dev/null &&
        dbus-send --session --type=signal /KIconLoader org.kde.KIconLoader.iconChanged int32:0 2>/dev/null || true
}

install_udev() {
    if cmp -s "$HOST_DIR/packaging/99-nerd-deck.rules" "$UDEV_RULE"; then
        info "udev rule $UDEV_RULE is up to date"
        return
    fi
    info "Installing udev rule to $UDEV_RULE (needs sudo)"
    sudo install -m644 "$HOST_DIR/packaging/99-nerd-deck.rules" "$UDEV_RULE"
    sudo udevadm control --reload
    sudo udevadm trigger --subsystem-match=tty --attr-match=idVendor=303a
}

install_service() {
    info "Installing and enabling $SERVICE"
    mkdir -p "$UNIT_DIR"
    sed "s|^ExecStart=.*|ExecStart=$BIN_DIR/nerd-deck|" "$HOST_DIR/packaging/$SERVICE" >"$UNIT_DIR/$SERVICE"
    systemctl --user daemon-reload
    systemctl --user enable "$SERVICE"
    # restart rather than start so a re-run picks up the new binary
    systemctl --user restart "$SERVICE"
}

install_tray() {
    info "Installing the tray icon to start at login"
    mkdir -p "$AUTOSTART_DIR"
    sed "s|^Exec=.*|Exec=$BIN_DIR/nerd-deck --tray|" "$HOST_DIR/packaging/$TRAY" >"$AUTOSTART_DIR/$TRAY"
    # Restart it now so a re-run picks up the new binary.
    pkill -u "$USER" -f "^[^ ]*nerd-deck --tray" || true
    if [[ -n ${WAYLAND_DISPLAY:-}${DISPLAY:-} ]]; then
        setsid -f "$BIN_DIR/nerd-deck" --tray >/dev/null 2>&1
    fi
}

if ((uninstall)); then
    uninstall
    exit 0
fi

check_deps
((do_firmware)) && build_firmware
install_binary
((do_udev)) && install_udev
((do_service)) && install_service
((do_tray)) && install_tray

info "Installed."
if ((do_service)); then
    echo "    Logs:    journalctl --user -u nerd-deck -f"
    echo "    Stop:    systemctl --user stop nerd-deck   (do this before flashing firmware)"
else
    echo "    Run:     $BIN_DIR/nerd-deck"
fi
