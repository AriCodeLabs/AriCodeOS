#!/bin/sh
# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# SPDX-License-Identifier: Apache-2.0
# Watches a machine under test through its `temp` command: logs each reading, says the
# maximum, and past the limit shuts the machine down. For heavy runs on the reference
# laptop (the owner's rule: every heavy test has a
# watch).
#
#   tools/watch-temperature.sh ari@192.168.1.27 [limit] [log] -- [ssh options]
set -u
target=$1
limit=${2:-92}
log=${3:-$HOME/aricode-logs/temperature.log}
[ $# -ge 3 ] && shift 3 || shift $#
[ "${1:-}" = "--" ] && shift
max=0
while :; do
    line=$(ssh -o BatchMode=yes -o ConnectTimeout=5 "$@" "$target" temp 2>/dev/null)
    now=$(echo "$line" | sed -n 's/^temperature: \([0-9]*\) C.*/\1/p')
    if [ -n "$now" ]; then
        [ "$now" -gt "$max" ] && max=$now
        echo "$(date +%T) $now C, max $max C" >>"$log"
        if [ "$now" -ge "$limit" ]; then
            echo "$(date +%T) $now C reached the limit of $limit C: shutting it down" | tee -a "$log"
            timeout 15 ssh -o BatchMode=yes "$@" "$target" shutdown
            exit 1
        fi
    fi
    sleep 5
done
