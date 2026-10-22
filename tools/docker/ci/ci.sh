#!/bin/sh
# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# SPDX-License-Identifier: Apache-2.0
#
# Continuous test runs of AriCodeOS (ADR 0040), inside the CI container.
#
#   aricodeos-ci [--branch B] [--repo URL] [--jobs N] [--loop] [--interval S] [--pause S] [--quick]
#
# Keeps its own clone of the repository in /work (a named volume, never a
# developer's checkout), checks out the head of the branch (default
# develop), runs `cargo xtask check`, `cargo xtask test --all` and
# `cargo xtask test --all --cpus 4` and writes a dated report under
# /reports: summary.txt (commit, steps, pass/fail per scenario of both
# passes), the full log of each step and the logs of every failed scenario.
# /reports/index.txt gets one line per run and /reports/latest points at
# the newest report. With --loop it polls the remote every --interval
# seconds (default 300) and runs once per new commit, resting --pause
# seconds (default 600) after each run so the machine cools down;
# otherwise it runs once and exits non-zero if a step failed. One suite at
# a time, every step at nice 10 so interactive work on the host comes
# first. --quick runs `check` and the default test level only: it is for
# trying the container itself.
#
# The repository is public and fetched over https: no credentials.
set -u

repo=${ARICODEOS_CI_REPO:-https://github.com/AriCodeLabs/AriCodeOS.git}
branch=${ARICODEOS_CI_BRANCH:-develop}
jobs=${ARICODEOS_CI_JOBS:-}
interval=${ARICODEOS_CI_INTERVAL:-300}
pause=${ARICODEOS_CI_PAUSE:-600}
loop=false
quick=false
work=/work
reports=/reports

while [ $# -gt 0 ]; do
    case "$1" in
        --branch) branch=$2; shift 2 ;;
        --repo) repo=$2; shift 2 ;;
        --jobs) jobs=$2; shift 2 ;;
        --interval) interval=$2; shift 2 ;;
        --pause) pause=$2; shift 2 ;;
        --loop) loop=true; shift ;;
        --quick) quick=true; shift ;;
        -h|--help) sed -n '5,26p' "$0"; exit 0 ;;
        *) echo "aricodeos-ci: unknown option $1 (see --help)" >&2; exit 2 ;;
    esac
done

case "$jobs" in
    *[!0-9]*) echo "aricodeos-ci: --jobs $jobs: not a number" >&2; exit 2 ;;
esac
for value in "$interval" "$pause"; do
    case "$value" in
        ''|*[!0-9]*) echo "aricodeos-ci: --interval/--pause $value: not a number of seconds" >&2; exit 2 ;;
    esac
done

src=$work/AriCodeOS
slug=$(printf '%s' "$branch" | tr -c 'A-Za-z0-9._-' '-')
last=$work/last-tested-$slug

log() {
    echo "[aricodeos-ci $(date -u +%Y-%m-%dT%H:%M:%SZ)] $*"
}

# Clones on first use, then fetches the branch; prints the head commit.
fetch() {
    if [ ! -d "$src/.git" ]; then
        log "cloning $repo into $src" >&2
        git clone --quiet --no-checkout "$repo" "$src" >&2 || return 1
    fi
    git -C "$src" remote set-url origin "$repo" &&
        git -C "$src" fetch --quiet origin "+refs/heads/$branch:refs/remotes/origin/$branch" >&2 &&
        git -C "$src" rev-parse "refs/remotes/origin/$branch"
}

# step NAME COMMAND...: runs the command in the clone, logs it to
# NAME.log in the report and records the result in steps.txt.
step() {
    name=$1
    shift
    log "$name: $*"
    t0=$(date +%s)
    (cd "$src" && nice -n 10 "$@") >"$dir/$name.log" 2>&1
    status=$?
    t=$(( $(date +%s) - t0 ))
    if [ $status -eq 0 ]; then result=PASS; else result=FAIL; failed_steps=$((failed_steps + 1)); fi
    printf '%-12s %s  %dm%02ds  %s\n' "$name" "$result" $((t / 60)) $((t % 60)) "$*" >>"$dir/steps.txt"
    log "$name: $result in $((t / 60))m$((t % 60))s"
}

# Keeps the PASS/FAIL line of every scenario of a test step and the logs
# of the failed ones (the next pass overwrites build/scenarios/).
collect() {
    name=$1
    grep -E '^\[(PASS|FAIL)\] ' "$dir/$name.log" >"$dir/$name.results" 2>/dev/null
    for scenario in $(grep -E '^\[FAIL\] ' "$dir/$name.log" | awk '{print $2}'); do
        mkdir -p "$dir/failed/$name/$scenario"
        cp "$src/build/scenarios/$scenario"/serial*.log "$src/build/scenarios/$scenario"/qemu-stderr*.log \
            "$dir/failed/$name/$scenario/" 2>/dev/null
    done
}

run_once() {
    sha=$1
    git -C "$src" checkout --quiet --force --detach "$sha" || return 1
    # Untracked files go; the build cache (target/) and the scenario
    # directories (their last run times order the next run) stay.
    git -C "$src" clean -ffdxq -e /target -e /build
    short=$(git -C "$src" rev-parse --short "$sha")
    dir=$reports/$(date -u +%Y-%m-%dT%H%MZ)-$slug-$short
    mkdir -p "$dir" || return 1
    failed_steps=0
    started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    run_start=$(date +%s)
    log "testing $branch at $short ($(git -C "$src" log -1 --format=%s "$sha"))"
    set -- ${jobs:+--jobs "$jobs"}
    step check cargo xtask check
    if $quick; then
        step test cargo xtask test "$@"
        collect test
        tests="test"
    else
        step test-1cpu cargo xtask test --all "$@"
        collect test-1cpu
        step test-4cpus cargo xtask test --all --cpus 4 "$@"
        collect test-4cpus
        tests="test-1cpu test-4cpus"
    fi
    total=$(( $(date +%s) - run_start ))
    if [ $failed_steps -eq 0 ]; then verdict=PASS; else verdict=FAIL; fi
    {
        echo "AriCodeOS continuous run: $verdict"
        echo "branch   $branch"
        echo "commit   $sha"
        echo "subject  $(git -C "$src" log -1 --format=%s "$sha")"
        echo "repo     $repo"
        echo "started  $started"
        echo "took     $((total / 60))m$((total % 60))s"
        echo "jobs     ${jobs:-default (cargo xtask test picks from CPUs and memory)}"
        echo
        echo "Steps:"
        cat "$dir/steps.txt"
        for name in $tests; do
            echo
            pass=$(grep -c '^\[PASS\]' "$dir/$name.results" 2>/dev/null)
            fail=$(grep -c '^\[FAIL\]' "$dir/$name.results" 2>/dev/null)
            echo "$name: ${pass:-0} passed, ${fail:-0} failed"
            grep -E '^\[xtask\] QEMU wall clock' "$dir/$name.log"
            cat "$dir/$name.results" 2>/dev/null
        done
        [ ! -d "$dir/failed" ] || { echo; echo "Logs of failed scenarios: failed/<step>/<scenario>/"; }
    } >"$dir/summary.txt"
    printf '%s %s %s %s %dm%02ds %s\n' "$started" "$branch" "$short" "$verdict" \
        $((total / 60)) $((total % 60)) "$(basename "$dir")" >>"$reports/index.txt"
    ln -sfn "$(basename "$dir")" "$reports/latest"
    echo "$sha" >"$last"
    log "$verdict: $dir/summary.txt"
    [ $failed_steps -eq 0 ]
}

mkdir -p "$work" "$reports" || exit 1
while :; do
    if sha=$(fetch); then
        if [ "$sha" != "$(cat "$last" 2>/dev/null)" ] || ! $loop; then
            run_once "$sha"
            status=$?
            $loop || exit $status
            log "resting ${pause}s before looking for the next commit"
            sleep "$pause"
        fi
    else
        log "cannot fetch $branch from $repo; retrying in ${interval}s"
        $loop || exit 1
    fi
    sleep "$interval"
done
