#!/bin/bash
# clipbrain installer
# Usage: ./install.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_ID="app.clipbrain"
LAUNCHER="$HOME/.local/bin/clipbrain"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
DESKTOP_FILE="$DATA_HOME/applications/$APP_ID.desktop"
ICON_DIR="$DATA_HOME/icons/hicolor"
KEYBINDING_PATH="/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/clipbrain/"

# ── Helpers ──────────────────────────────────────────────────────────────────

green()  { printf '\033[0;32m%s\033[0m\n' "$*"; }
yellow() { printf '\033[0;33m%s\033[0m\n' "$*"; }
red()    { printf '\033[0;31m%s\033[0m\n' "$*"; }
step()   { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }

require_apt() {
    local missing=()
    for pkg in "$@"; do
        if ! dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null | grep -q "install ok installed"; then
            missing+=("$pkg")
        fi
    done
    if [ ${#missing[@]} -eq 0 ]; then
        green "  All required system packages already installed."
        return
    fi
    yellow "  Missing packages: ${missing[*]}"
    echo "  Installing via apt (sudo required)..."
    sudo apt-get update -qq
    sudo apt-get install -y "${missing[@]}"
    green "  System packages installed."
}

# ── 1. Build dependencies ────────────────────────────────────────────────────

step "Checking system dependencies"

BUILD_DEPS=(libgtk-4-dev libadwaita-1-dev pkg-config build-essential)

if ! command -v apt-get &>/dev/null; then
    red "This installer supports Debian/Ubuntu-based systems (apt)."
    red "Please install the equivalents of these packages manually and re-run:"
    red "  ${BUILD_DEPS[*]}"
    red "(GTK >= 4.22 and libadwaita >= 1.9 development files, pkg-config, a C toolchain)"
    exit 1
fi

require_apt "${BUILD_DEPS[@]}"

# ── 2. Rust toolchain ────────────────────────────────────────────────────────

step "Checking Rust toolchain"

if ! command -v cargo &>/dev/null && [ -f "$HOME/.cargo/env" ]; then
    # rustup is installed but not on this shell's PATH yet.
    # shellcheck source=/dev/null
    . "$HOME/.cargo/env"
fi

if command -v cargo &>/dev/null; then
    green "  cargo found: $(command -v cargo) ($(cargo --version))"
else
    yellow "  cargo not found — installing rustup (minimal profile)..."
    if ! command -v curl &>/dev/null; then
        require_apt curl
    fi
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    # shellcheck source=/dev/null
    . "$HOME/.cargo/env"
    green "  Rust installed: $(cargo --version)"
fi

# ── 3. Build ─────────────────────────────────────────────────────────────────

step "Building clipbrain (release)"

cd "$SCRIPT_DIR"
cargo build --release --locked
green "  Built: $SCRIPT_DIR/target/release/clipbrain"

# ── 4. Install binary, desktop file and icon ─────────────────────────────────

step "Installing"

install -Dm755 target/release/clipbrain "$LAUNCHER"
green "  Installed: $LAUNCHER"

install -Dm644 resources/icons/app.clipbrain.svg "$ICON_DIR/scalable/apps/$APP_ID.svg"
green "  Installed: $ICON_DIR/scalable/apps/$APP_ID.svg"

mkdir -p "$(dirname "$DESKTOP_FILE")"
sed "s|^Exec=.*|Exec=$LAUNCHER|" resources/app.clipbrain.desktop > "$DESKTOP_FILE"
chmod 644 "$DESKTOP_FILE"
green "  Installed: $DESKTOP_FILE"

if command -v update-desktop-database &>/dev/null; then
    update-desktop-database -q "$(dirname "$DESKTOP_FILE")" || true
fi

# ── 5. GNOME keyboard shortcut (optional) ────────────────────────────────────

_register_keybinding() {
    local kpath="$1" name="$2" cmd="$3" binding="$4"
    local EXISTING NEW_LIST
    EXISTING=$(gsettings get org.gnome.settings-daemon.plugins.media-keys custom-keybindings 2>/dev/null || echo "@as []")
    if ! echo "$EXISTING" | grep -q "$kpath"; then
        if [ "$EXISTING" = "@as []" ] || [ "$EXISTING" = "[]" ]; then
            NEW_LIST="['$kpath']"
        else
            NEW_LIST="${EXISTING%']'},'$kpath']"
        fi
        gsettings set org.gnome.settings-daemon.plugins.media-keys custom-keybindings "$NEW_LIST"
    fi
    gsettings set "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:$kpath" name    "$name"
    gsettings set "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:$kpath" command "$cmd"
    gsettings set "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:$kpath" binding "$binding"
    green "  Registered shortcut '$binding' → $name"
}

if ! command -v gsettings &>/dev/null; then
    echo ""
    yellow "gsettings not available — skipping GNOME keyboard shortcuts."
else
    step "GNOME keyboard shortcut to show the history window (optional)"
    echo "  The app binds Ctrl+Shift+V internally."
    echo "  You can also register a system-level shortcut to launch/focus it:"
    echo "    1) <Super>V"
    echo "    2) <Control><Shift>V"
    echo "    3) Skip"
    read -rp "  Choice [1/2/3]: " choice || choice=3
    case "$choice" in
        1) _register_keybinding "$KEYBINDING_PATH" "Clipbrain" "$LAUNCHER" "<Super>v" ;;
        2) _register_keybinding "$KEYBINDING_PATH" "Clipbrain" "$LAUNCHER" "<Control><Shift>v" ;;
        *) yellow "  Skipped." ;;
    esac
fi

# ── 6. Autostart (optional) ──────────────────────────────────────────────────

step "Autostart on login (optional)"

AUTOSTART_DIR="$HOME/.config/autostart"
AUTOSTART_FILE="$AUTOSTART_DIR/clipbrain.desktop"

echo "  Install a .desktop file to start clipbrain on login?"
echo "    1) Yes"
echo "    2) No"
read -rp "  Choice [1/2]: " choice_as || choice_as=2
if [ "$choice_as" = "1" ]; then
    mkdir -p "$AUTOSTART_DIR"
    {
        cat "$DESKTOP_FILE"
        echo "X-GNOME-Autostart-enabled=true"
    } > "$AUTOSTART_FILE"
    green "  Autostart installed: $AUTOSTART_FILE"
else
    yellow "  Skipped."
fi

# ── Done ─────────────────────────────────────────────────────────────────────

printf '\n'
green "╔══════════════════════════════════════════╗"
green "║     clipbrain installed successfully!    ║"
green "╚══════════════════════════════════════════╝"
echo ""
echo "  If an older clipbrain is still running, quit it from its tray icon"
echo "  first; a running instance keeps handling launches until it exits."
echo ""
echo "  Start it now:"
echo "    clipbrain"
echo ""
echo "  If 'clipbrain' is not found, add ~/.local/bin to your PATH:"
echo "    echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.bashrc && source ~/.bashrc"
echo ""
echo "  The app runs in the background with a system tray icon."
echo "  Use the tray icon or your shortcut to show the history window."
