#!/bin/bash
# clipbrain installer
# Usage: ./install.sh
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAUNCHER="$HOME/.local/bin/clipbrain"
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

# ── 1. Detect distro ─────────────────────────────────────────────────────────

step "Checking system dependencies"

if ! command -v apt-get &>/dev/null; then
    red "This installer supports Debian/Ubuntu-based systems (apt)."
    red "Please install the following packages manually and re-run:"
    red "  libgirepository-2.0-dev libgtk-4-dev gir1.2-gtk-4.0"
    red "  libgtk-3-dev gir1.2-gtk-3.0"
    red "  libayatana-appindicator3-1 gir1.2-ayatanaappindicator3-0.1"
    red "  python3-gi python3-gi-cairo python3-dev"
    exit 1
fi

require_apt \
    libgirepository-2.0-dev \
    libgtk-4-dev \
    gir1.2-gtk-4.0 \
    libadwaita-1-dev \
    gir1.2-adw-1 \
    libgtk-3-dev \
    gir1.2-gtk-3.0 \
    libayatana-appindicator3-1 \
    gir1.2-ayatanaappindicator3-0.1 \
    python3-gi \
    python3-gi-cairo \
    python3-dev

# ── 2. Ensure uv is installed ────────────────────────────────────────────────

step "Checking uv"

UV_BIN="$HOME/.local/bin/uv"

if command -v uv &>/dev/null; then
    UV_BIN="$(command -v uv)"
    green "  uv found: $UV_BIN ($(uv --version))"
elif [ -x "$UV_BIN" ]; then
    green "  uv found: $UV_BIN ($("$UV_BIN" --version))"
else
    yellow "  uv not found — installing via official installer..."
    curl -LsSf https://astral.sh/uv/install.sh | sh
    if [ ! -x "$UV_BIN" ]; then
        red "uv installation failed or was placed somewhere unexpected."
        red "Please install uv manually: https://docs.astral.sh/uv/getting-started/installation/"
        exit 1
    fi
    green "  uv installed: $UV_BIN ($("$UV_BIN" --version))"
    yellow "  NOTE: Add ~/.local/bin to your PATH if it isn't already:"
    yellow "    echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.bashrc"
fi

# ── 3. Install Python environment ────────────────────────────────────────────

step "Installing Python environment"

cd "$SCRIPT_DIR"
"$UV_BIN" pip install -e .
green "  Python environment ready."

# ── 4. Install launcher ──────────────────────────────────────────────────────

step "Installing launcher"

mkdir -p "$HOME/.local/bin"

cat > "$LAUNCHER" <<EOF
#!/bin/bash
cd "$SCRIPT_DIR" && "$UV_BIN" run clipbrain "\$@"
EOF
chmod +x "$LAUNCHER"
green "  Installed: $LAUNCHER"

# ── 5. GNOME keyboard shortcut (optional) ────────────────────────────────────

_register_keybinding() {
    local kpath="$1" name="$2" cmd="$3" binding="$4"
    local EXISTING
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
    yellow "\ngsettings not available — skipping GNOME keyboard shortcuts."
else
    step "GNOME keyboard shortcut to show the history window (optional)"
    echo "  The app binds Ctrl+Shift+V internally."
    echo "  You can also register a system-level shortcut to launch/focus it:"
    echo "    1) <Super>V"
    echo "    2) <Control><Shift>V"
    echo "    3) Skip"
    read -rp "  Choice [1/2/3]: " choice
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
read -rp "  Choice [1/2]: " choice_as
if [ "$choice_as" = "1" ]; then
    mkdir -p "$AUTOSTART_DIR"
    cat > "$AUTOSTART_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=Clipbrain
Comment=Clipboard history manager
Exec=$LAUNCHER
Icon=edit-paste-symbolic
Hidden=false
NoDisplay=false
X-GNOME-Autostart-enabled=true
EOF
    green "  Autostart installed: $AUTOSTART_FILE"
else
    yellow "  Skipped."
fi

# ── Done ─────────────────────────────────────────────────────────────────────

printf '\n'
green "╔══════════════════════════════════════════╗"
green "║  clipbrain installed successfully!  ║"
green "╚══════════════════════════════════════════╝"
echo ""
echo "  Start it now:"
echo "    clipbrain"
echo ""
echo "  If 'clipbrain' is not found, add ~/.local/bin to your PATH:"
echo "    echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.bashrc && source ~/.bashrc"
echo ""
echo "  The app runs in the background with a system tray icon."
echo "  Use the tray icon or Ctrl+Shift+V to show the history window."
