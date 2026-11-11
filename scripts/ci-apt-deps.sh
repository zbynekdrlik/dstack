#!/usr/bin/env bash
# The Linux build's system headers (GTK, AppIndicator, ALSA: dtray's tray,
# dlisten's output), one script for every CI and release job.
set -euo pipefail

sudo apt-get -o Acquire::Retries=3 update
sudo apt-get -o Acquire::Retries=3 install -y libgtk-3-dev libayatana-appindicator3-dev libasound2-dev
