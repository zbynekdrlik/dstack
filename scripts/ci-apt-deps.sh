#!/usr/bin/env bash
# The Linux build's system headers (GTK, AppIndicator, ALSA: dtray's tray,
# dlisten's output), one script for every CI and release job.
#
# GitHub's Ubuntu runners list azure.archive.ubuntu.com first in
# /etc/apt/apt-mirrors.txt. On 11.11.2026 that name stopped resolving for
# hours, apt waited ~20 s per package on it, and every job hit its timeout.
# The runner's own list goes on to archive.ubuntu.com, so the Azure mirror
# is dropped from it: apt then asks the main archive at once.
set -euo pipefail

mirrors=/etc/apt/apt-mirrors.txt
if [ -f "$mirrors" ] && grep -q 'azure.archive.ubuntu.com' "$mirrors"; then
    sudo sed -i '/azure\.archive\.ubuntu\.com/d' "$mirrors"
    if ! grep -q 'archive.ubuntu.com' "$mirrors"; then
        echo 'http://archive.ubuntu.com/ubuntu/' | sudo tee -a "$mirrors" >/dev/null
    fi
    echo "ci-apt-deps: apt mirrors now:"
    cat "$mirrors"
fi
sudo apt-get -o Acquire::Retries=3 update
sudo apt-get -o Acquire::Retries=3 install -y libgtk-3-dev libayatana-appindicator3-dev libasound2-dev
