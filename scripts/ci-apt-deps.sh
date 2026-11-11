#!/usr/bin/env bash
# The Linux build's system headers (GTK, AppIndicator, ALSA: dtray's tray,
# dlisten's output), one script for every CI and release job.
#
# On 11.11.2026 GitHub's Ubuntu runners reached neither azure.archive.ubuntu.com
# (unresolved) nor archive.ubuntu.com (timeouts) for hours, while the rest of
# the internet worked; apt waited ~20 s a package and every job hit its
# timeout. So apt gets short timeouts, and when the runner's own mirrors fail
# it tries another public Ubuntu mirror before giving up.
set -euo pipefail

PKGS=(libgtk-3-dev libayatana-appindicator3-dev libasound2-dev)
FALLBACK=http://mirrors.edge.kernel.org/ubuntu/
APT=(sudo apt-get -o Acquire::Retries=2 -o Acquire::http::Timeout=15 -o Acquire::https::Timeout=15)

install() {
    "${APT[@]}" update --error-on=any && "${APT[@]}" install -y "${PKGS[@]}"
}

if install; then
    exit 0
fi
echo "ci-apt-deps: the runner's Ubuntu mirrors failed; trying ${FALLBACK}" >&2
mirrors=/etc/apt/apt-mirrors.txt
if [ -f "$mirrors" ]; then
    echo "$FALLBACK" | sudo tee "$mirrors" >/dev/null
fi
for f in /etc/apt/sources.list /etc/apt/sources.list.d/ubuntu.sources; do
    if [ -f "$f" ]; then
        sudo sed -i -E "s#https?://(azure\.)?archive\.ubuntu\.com/ubuntu/?#${FALLBACK}#g; s#https?://security\.ubuntu\.com/ubuntu/?#${FALLBACK}#g" "$f"
    fi
done
install
