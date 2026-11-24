#!/bin/bash
# Runs the installed Flatpak (.github/workflows/flatpak.yml): through its
# helper it sees the computer's processes, not only the sandbox's.
set -euo pipefail
id=org.patrone.TaskManager
# flatpak-spawn --host reaches the computer over the session bus
listing=$(dbus-run-session -- flatpak run "$id" --list-processes)
echo "$listing" | head -5
count=$(wc -l <<< "$listing")
echo "$count processes"
# The computer's PID 1 is its init (systemd), where the sandbox's is bwrap
if ! grep -qP '^1\t\S+\t(systemd|init)$' <<< "$listing"; then
    echo "Not the computer's processes. The helper, run by hand:" >&2
    dbus-run-session -- flatpak run --command=sh "$id" -c \
        'files=$(sed -n "s/^app-path=//p" /.flatpak-info); echo "app-path: $files"; printf "R\t/proc/1/comm\n" | flatpak-spawn --host "$files/libexec/patrone-task-manager-agent"' >&2 || true
    exit 1
fi
[[ $count -gt 20 ]]
flatpak run --command=ls "$id" /app/share/doc/patrone-task-manager/third-party-licenses.txt.gz \
    /app/share/metainfo/org.patrone.TaskManager.metainfo.xml
echo "The Flatpak sees the computer's processes."
