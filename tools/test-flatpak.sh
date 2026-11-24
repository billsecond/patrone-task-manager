#!/bin/bash
# Runs the installed Flatpak (.github/workflows/flatpak.yml): through its
# helper it sees the computer's processes, not only the sandbox's.
set -euo pipefail
id=org.patrone.TaskManager
listing=$(flatpak run "$id" --list-processes)
echo "$listing" | head -5
count=$(wc -l <<< "$listing")
echo "$count processes"
# The computer's PID 1 is its init (systemd), where the sandbox's is bwrap
grep -qP '^1\t\S+\t(systemd|init)$' <<< "$listing"
[[ $count -gt 20 ]]
# The helper is linked statically
flatpak run --command=sh "$id" -c 'file /app/libexec/patrone-task-manager-agent 2> /dev/null || true'
flatpak run --command=ls "$id" /app/share/doc/patrone-task-manager/third-party-licenses.txt.gz \
    /app/share/metainfo/org.patrone.TaskManager.metainfo.xml
echo "The Flatpak sees the computer's processes."
