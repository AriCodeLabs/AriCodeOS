#!/usr/bin/env python3
"""Bounded audit probes; extracted production code, temporary host artifacts only."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]


def rust_item(text, start):
    begin = text.index(start)
    opening = text.index('{', begin)
    depth = 1
    end = opening + 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[begin:end]


with tempfile.TemporaryDirectory(prefix='ari-audit-2943a1e-') as temporary:
    tmp = Path(temporary)
    qemu = (ROOT / 'tools/xtask/src/qemu.rs').read_text()
    items = '\n'.join(rust_item(qemu, start) for start in [
        'struct Servers {', 'pub struct UpdateServer {', 'impl UpdateServer {',
        'fn accept_loop(', 'impl Drop for Servers {',
    ])
    # No request is sent: only the HTTP payload handler is stubbed.
    source = '''
use std::{path::Path, time::Duration};
type Result<T = ()> = std::result::Result<T, String>;
#[path = "PORTS_PATH"] mod ports;
fn http_serve(_: std::net::TcpStream, _: Option<&Path>) {}
ITEMS
fn main() {
    // 64 jobs with two forwards each; amplify the same allocation race.
    let leases: Vec<_> = (0..64).map(|_| ports::PortLease::take(2).unwrap()).collect();
    let reserved: Vec<_> = leases.iter().flat_map(|l| l.ports()).copied().collect();
    for attempt in 1..=4096 {
        let server = UpdateServer::start(Path::new("unused")).unwrap();
        if reserved.contains(&server.port()) {
            let error = std::net::TcpListener::bind(("127.0.0.1", server.port())).unwrap_err();
            println!("PORT COLLISION: update server took leased port {} after {} allocations; forward bind: {}", server.port(), attempt, error);
            assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
            return;
        }
    }
    panic!("no collision in bounded sample; this probabilistic probe is inconclusive");
}
'''.replace('PORTS_PATH', str(ROOT / 'tools/xtask/src/ports.rs')).replace('ITEMS', items)
    (tmp / 'ports.rs').write_text(source)
    subprocess.run(['rustc', '--edition=2021', str(tmp / 'ports.rs'), '-o', str(tmp / 'ports')], check=True, cwd=ROOT)
    subprocess.run([str(tmp / 'ports')], check=True, timeout=100)

    ci = (ROOT / 'tools/docker/ci/ci.sh').read_text()
    functions = ci[ci.index('log() {'):ci.index('mkdir -p "$work" "$reports" || exit 1')]
    # Exercise the real run_once/step/collect functions. Mock only time, Git
    # and expensive commands. Both calls occur in the same report minute.
    shell = '''#!/bin/sh
set -u
base=$1
src=$base/src
reports=$base/reports
last=$base/last-tested
repo=mock-repository
branch=develop
slug=develop
jobs=
quick=true
mkdir -p "$src/build/scenarios/probe.failed" "$reports"
printf 'first failure evidence\\n' > "$src/build/scenarios/probe.failed/serial.log"
printf 'first stderr evidence\\n' > "$src/build/scenarios/probe.failed/qemu-stderr.log"
git() {
    case "$*" in
        *rev-parse*) printf '2943a1e\\n' ;;
        *log*) printf 'audit fixture\\n' ;;
    esac
    return 0
}
date() {
    case "$*" in
        *%s*) printf '1000000\\n' ;;
        *%H%MZ*) printf '2026-10-23T0100Z\\n' ;;
        *) printf '2026-10-23T01:00:00Z\\n' ;;
    esac
}
nice() {
    if [ "$mode" = fail ]; then
        printf '[FAIL] probe.failed 0.0s simulated failure\\n'
        return 1
    fi
    printf '[PASS] probe.passed 0.0s ok\\n'
    return 0
}
FUNCTIONS
mode=fail
run_once 2943a1e
first=$?
first_dir=$dir
cp "$dir/summary.txt" "$base/first-summary.txt"
mode=pass
run_once 2943a1e
second=$?
printf 'CI REUSE: first exit=%s, second exit=%s, same directory=%s\\n' "$first" "$second" "$([ "$dir" = "$first_dir" ] && echo yes || echo no)"
printf 'Index entries: '; wc -l < "$reports/index.txt"
printf 'Accumulated step entries: '; wc -l < "$dir/steps.txt"
cat "$dir/summary.txt"
[ "$first" -eq 1 ] && [ "$second" -eq 0 ] && [ "$dir" = "$first_dir" ] &&
    grep -q '^AriCodeOS continuous run: PASS' "$dir/summary.txt" &&
    grep -q 'FAIL' "$dir/steps.txt" &&
    [ -f "$dir/failed/test/probe.failed/serial.log" ]
'''.replace('FUNCTIONS', functions)
    (tmp / 'ci-probe.sh').write_text(shell)
    subprocess.run(['sh', str(tmp / 'ci-probe.sh'), str(tmp)], check=True)
