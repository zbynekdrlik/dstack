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
APT=(sudo apt-get -o Acquire::Retries=2 -o Acquire::http::Timeout=15 -o Acquire::https::Timeout=15 -o Acquire::ForceIPv4=true)

# Every package here comes from Ubuntu's own archive. A third-party source
# the runner image carries (packages.microsoft.com and others under
# sources.list.d) only adds a way to fail: on 18.11.2026 the Microsoft repo
# answered 429 and `--error-on=any` failed the v0.6.179 release's Linux job
# while the Ubuntu mirrors were fine. Leave them out of this job's apt.
for f in /etc/apt/sources.list.d/*; do
    case "$f" in
        */ubuntu.sources) ;;
        *) [ -e "$f" ] && sudo mv "$f" "$f.ci-apt-deps-off" ;;
    esac
done

# A mirror can also answer, but at seconds a package: on 18.11.2026 a
# runner's us-east-2.ec2.archive.ubuntu.com took 9-10 min for this step
# (the azure mirror ~1 min) and the fmt job hit its 10 min limit twice. So
# one try is bounded too (4 min, about 4x a normal run), and a try that
# ran out of time moves to the fallback mirror like one that failed.
TRY_SECONDS=240

install() {
    timeout "$TRY_SECONDS" "${APT[@]}" update --error-on=any &&
        timeout "$TRY_SECONDS" "${APT[@]}" install -y "${PKGS[@]}"
}

if install; then
    exit 0
fi
echo "ci-apt-deps: the runner's Ubuntu mirrors failed or ran out of ${TRY_SECONDS} s; trying ${FALLBACK}" >&2
mirrors=/etc/apt/apt-mirrors.txt
if [ -f "$mirrors" ]; then
    echo "$FALLBACK" | sudo tee "$mirrors" >/dev/null
fi
# A try stopped by its time limit may have left dpkg half done.
sudo dpkg --configure -a
for f in /etc/apt/sources.list /etc/apt/sources.list.d/ubuntu.sources; do
    if [ -f "$f" ]; then
        sudo sed -i -E "s#https?://([a-z0-9-]+\.)*archive\.ubuntu\.com/ubuntu/?#${FALLBACK}#g; s#https?://security\.ubuntu\.com/ubuntu/?#${FALLBACK}#g" "$f"
    fi
done
install
