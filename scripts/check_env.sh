#!/usr/bin/env bash

# Load .env if present
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="$SCRIPT_DIR/../.env"
if [ -f "$ENV_FILE" ]; then
    set -a
    # shellcheck disable=SC1090
    source "$ENV_FILE"
    set +a
fi

# Color codes
RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m' # No Color

PASS=0
FAIL=0

check_pass() {
    echo -e "${GREEN}PASS${NC}: $1"
    ((PASS++))
}

check_fail() {
    echo -e "${RED}FAIL${NC}: $1"
    ((FAIL++))
}

# 1. Check XDG_SESSION_TYPE is wayland
if [ "$XDG_SESSION_TYPE" = "wayland" ]; then
    check_pass "XDG_SESSION_TYPE is wayland"
else
    check_fail "XDG_SESSION_TYPE is '$XDG_SESSION_TYPE', not wayland"
fi

# 2. Check tesseract is in PATH and runs
if command -v tesseract &> /dev/null && tesseract --version &> /dev/null; then
    check_pass "tesseract found and working"
else
    check_fail "tesseract not found or not working"
fi

# 3. Check grim or gnome-screenshot in PATH
if command -v grim &> /dev/null; then
    check_pass "grim found"
elif command -v gnome-screenshot &> /dev/null; then
    check_pass "gnome-screenshot found"
else
    check_fail "neither grim nor gnome-screenshot found"
fi

# 4. Check pactl info works
if pactl info &> /dev/null; then
    check_pass "pactl info works (PipeWire/PulseAudio)"
else
    check_fail "pactl info failed"
fi

# 5. Check xdg-desktop-portal service
if systemctl --user status xdg-desktop-portal &> /dev/null; then
    check_pass "xdg-desktop-portal service running"
else
    check_fail "xdg-desktop-portal service not running"
fi

# 6. Check xdg-desktop-portal-gnome service
if systemctl --user status xdg-desktop-portal-gnome &> /dev/null; then
    check_pass "xdg-desktop-portal-gnome service running"
else
    check_fail "xdg-desktop-portal-gnome service not running"
fi

# 7. Check python3 is installed
if command -v python3 &> /dev/null; then
    check_pass "python3 found"
else
    check_fail "python3 not found"
fi

# 8. Check OPENAI_API_KEY is set and non-empty
if [ -n "${OPENAI_API_KEY:-}" ]; then
    check_pass "OPENAI_API_KEY set"
else
    check_fail "OPENAI_API_KEY not set or empty"
fi

# Exit status
echo ""
echo "Summary: $PASS passed, $FAIL failed"
if [ $FAIL -eq 0 ]; then
    exit 0
else
    exit 1
fi
