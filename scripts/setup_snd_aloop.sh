#!/bin/bash
#
# snd-aloop setup script for RODECaster Virtual Audio Device
# ===========================================================
#
# This script loads the snd-aloop kernel module configured for
# the 16 subdevices needed by the RODECaster VAD (11 capture + 5 playback).
#
# The snd-aloop module creates virtual ALSA loopback sound cards.
# Each subdevice = one ALSA PCM device. The RODECaster VAD daemon
# bridges audio between USB RODE hardware and these virtual devices.
#
# Usage:
#   sudo ./setup_snd_aloop.sh
#
# To persist across reboots:
#   sudo cp setup_snd_aloop.sh /usr/local/bin/
#   echo 'options snd-aloop pcm_substreams=16' | sudo tee /etc/modprobe.d/rodecaster-vad.conf
#   echo 'snd-aloop' | sudo tee -a /etc/modules-load.d/rodecaster-vad.conf

set -euo pipefail

ALOOP_SUBDEVICES=16
ALOOP_CARD_ID="RODECaster"

echo "=== RODECaster VAD — snd-aloop Setup ==="
echo

# Check if running as root
if [[ $EUID -ne 0 ]]; then
    echo "ERROR: This script must be run as root (sudo)."
    echo "The snd-aloop module needs root to load."
    exit 1
fi

# 1. Check if snd-aloop is already loaded
if lsmod | grep -q "^snd_aloop"; then
    echo "[*] snd-aloop is already loaded."
    CURRENT_PARAMS=$(cat /sys/module/snd_aloop/parameters/pcm_substreams 2>/dev/null || echo "unknown")
    echo "    Current pcm_substreams: $CURRENT_PARAMS"

    if [[ "$CURRENT_PARAMS" != "$ALOOP_SUBDEVICES" ]]; then
        echo "[!] pcm_substreams=$CURRENT_PARAMS, but $ALOOP_SUBDEVICES needed."
        echo "    Unloading snd-aloop to reload with correct parameters..."
        if rmmod snd_aloop 2>/dev/null; then
            echo "    Unloaded."
        else
            echo "    WARNING: Could not unload snd-aloop. It may be in use."
            echo "    Stop any audio applications using the loopback devices first."
            echo "    Continuing with current configuration..."
        fi
    else
        echo "    Configuration is correct (pcm_substreams=$ALOOP_SUBDEVICES)."
        echo "[✓] snd-aloop is ready."
        echo
        echo "Available ALSA loopback cards:"
        arecord -l 2>/dev/null | grep -i loopback || echo "  (none — check /proc/asound/cards)"
        echo
        exit 0
    fi
fi

# 2. Load snd-aloop with the correct number of subdevices
echo "[*] Loading snd-aloop with pcm_substreams=$ALOOP_SUBDEVICES..."
modprobe snd-aloop pcm_substreams=$ALOOP_SUBDEVICES

# 3. Verify it loaded
sleep 0.5
if lsmod | grep -q "^snd_aloop"; then
    echo "[✓] snd-aloop loaded successfully."
    echo
else
    echo "[✗] Failed to load snd-aloop."
    echo "    Ensure the kernel module exists: find /lib/modules -name 'snd-aloop*'"
    exit 1
fi

# 4. Show cards
echo "Available ALSA loopback cards:"
cat /proc/asound/cards | grep -i loopback || echo "  (not found in /proc/asound/cards — may need a moment)"

echo
echo "ALSA PCM devices created by snd-aloop:"
arecord -l 2>/dev/null | grep -i loopback
aplay -l 2>/dev/null | grep -i loopback

echo
echo "[✓] Setup complete!"
echo
echo "You can now start the RODECaster VAD daemon:"
echo "  sudo systemctl start rodecaster-vad"
echo "  # or directly:"
echo "  cargo run"
echo
echo "The virtual ALSA devices will appear as:"
echo "  hw:Loopback,0,0  hw:Loopback,0,1  hw:Loopback,0,2  ..."
echo "  (up to 16 subdevices on card Loopback)"
echo
echo "To persist across reboots:"
echo "  echo 'options snd-aloop pcm_substreams=16' | sudo tee /etc/modprobe.d/rodecaster-vad.conf"
echo "  echo 'snd-aloop' | sudo tee -a /etc/modules-load.d/rodecaster-vad.conf"
