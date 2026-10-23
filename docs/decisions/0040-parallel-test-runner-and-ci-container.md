# ADR 0040: Parallel test runner and CI container

- **Date:** 2026-10-23
- **Status:** Accepted (amends ADR 0029's "scenarios publish into
  `build/update-test/` only": each scenario now publishes into its own
  directory, still never into `build/update/`)

## Context

`cargo xtask test --all` booted its 165 QEMU scenarios one after another,
and the release check runs it twice (`--all` and `--all --cpus 4`): about
an hour on the reference host (16 CPUs, 32 GiB), most of it with fifteen
CPUs idle. The owner's rule (docs/WORKFLOW.md, "Test levels per branch")
keeps the full suite for `main`, but an hour per release check and per
audit follow-up still slowed development, and nothing ran the suite on
`develop` between releases.

Two suites on one machine also collided several times: every host-side
port the runner used was fixed (UDP responder 17007, TCP echo 17008,
`hostfwd` 17009 and 17022, HTTP 17010, HTTPS 17443 and 17444), so a second
suite failed to bind, or worse, a scenario talked to the other suite's
server (an HTTPS server with another CA, an update channel with other
contents) and failed with a misleading error.

## Decision

1. **Per-run host ports where the guest does not have them compiled in.**
   QEMU's two `hostfwd` ports, the HTTPS servers of `host_https` and the
   HTTP server of `[host_update]` take ports the kernel picks (`bind` to
   port 0; `ports::PortLease` remembers them until the run ends so two runs
   of a suite never share one). The guest reaches a host port P at
   10.0.2.2:P, so scenarios write those ports as placeholders
   (`{update_port}`, `{https_port}`, `{https_untrusted_port}`) in their
   console lines, `kill_on` and expectations, and the runner fills them in;
   a placeholder without the field that starts its server is refused when
   the scenarios load. The `aricode_proto::net` constants for the forwarded
   and HTTPS ports are gone: no guest code used them. `cargo xtask run`
   keeps 17009 and 17022, so `ssh -p 17022 ari@127.0.0.1` still reaches an
   interactive machine.

2. **The three compiled-in ports stay fixed, one set per suite.** The test
   programs (`net_client`, `tcp_client`, `tcp_hog`, `sock_leaker`,
   `http_client`) and the typed `http http://10.0.2.2:17010/...` commands
   use 17007 (UDP), 17008 and 17010. QEMU's user network sends traffic for
   10.0.2.2:P to the host's 127.0.0.1:P and cannot remap it: `guestfwd`
   refuses the virtual host address (checked with QEMU 10.0 / libslirp
   4.8: "Conflicting/invalid host:port in guest forwarding rule") and has
   no UDP at all. Moving the virtual host would change what every scenario
   sees (router, ping, sshd's peer). These servers are stateless, so the
   suite starts them once (`qemu::Responder`), before building, and every
   scenario shares them; a second suite in the same network namespace stops
   at once with a message that says so and points to Docker. Per-run
   network namespaces would remove this last limit but need unprivileged
   user namespaces, which Docker's default seccomp profile refuses. The
   fixed HTTP server no longer serves `/update/`.

3. **Concurrent scenarios, deterministic output.** `cargo xtask test
   --jobs N` runs N scenarios at a time (`parallel::run_ordered`). The
   default is one run per CPU the process may use (`available_parallelism`,
   which follows a container's `--cpus`), at most 10, and two thirds of
   that with `--cpus` above 1, at most 6 (each such guest keeps several
   host threads busy under TCG): the owner's limits for the reference host
   (2026-10-23), which also runs test containers with `--cpus 10 --memory
   12g --memory-swap 12g` and one suite at a time. Never more runs than the
   available memory holds at 1 GiB each (`MemAvailable`, or a cgroup's
   `memory.max` minus what it uses beyond reclaimable page cache), an
   explicit `--jobs` included; measured QEMU resident sets stayed near
   160 MiB, so the budget is wide. `--jobs 1` is the old sequential loop.
   With several jobs the scenarios marked `serial = true` start first and
   run with nothing else going (point 4), then the longest (their previous
   run time, kept in `build/scenarios/<name>/elapsed`, or their timeout),
   so no long scenario starts last. Each scenario's output (its PASS/FAIL line, QEMU's
   stderr, now captured per run in `qemu-stderr.log`, the log of a failure)
   is printed as one block in listed order whatever finished first,
   followed by the wall clock, the summed scenario time and the unchanged
   summary line and exit code. An error of the runner itself (not a failed
   scenario) stops new runs and is reported after the results before it,
   as the sequential loop did.

4. **Nothing shared is written while scenarios run.** Each scenario writes
   only under `build/scenarios/<name>/`, its update source included
   (`update/`, served by its own server). The base ESP is built once before
   the runs, and `publish_channel` publishes a channel without rebuilding
   it (`publish_update_to` still builds for `publish-update` and
   `serve-update`). The external packages every selected scenario needs are
   built first and kept in memory by `build_external`, so no scenario starts
   cargo. `stripped_elf` strips into a file of its own per call (every
   scenario strips the kernel for its test disk). The update scenarios,
   which shared `build/update-test/` and the HTTP server, no longer share
   anything and run concurrently. Only timing can still couple runs: a
   scenario whose host side assumes the guest keeps up in wall-clock time
   is marked `serial = true` with the reason in its file. One was:
   `usb.hid_fault` (incident 0019), until the runner typed by the guest's
   echo instead of the clock (incident 0022).

5. **A CI container for `develop`.** `tools/docker/ci/` adds to the
   development image a script that keeps its own clone of the public
   repository (https, no credentials) in a named volume, never a
   developer's checkout, and on each new commit of a branch (default
   `develop`, polled every 5 minutes with `--loop`) runs `cargo xtask
   check`, `cargo xtask test --all` and `--all --cpus 4`, then writes a
   dated report (summary with pass/fail per scenario of both passes, step
   logs, logs of the failed scenarios) to a mounted directory, plus
   `index.txt` and `latest`. It runs one suite at a time, every step at
   nice 10, rests 10 minutes after each run (`--pause`), and is started
   with `--cpus 10 --memory 12g --memory-swap 12g --cpu-shares 512`; the
   runner sizes its jobs from those limits, so development continues
   beside it. A failure in its report is an incident.

## Results

Reference host (16 CPUs, 32 GiB), inside the development image limited to
`--cpus 10 --memory 12g --memory-swap 12g`, 165 scenarios, every one
passing. Times are of the whole command (unit tests, build, QEMU); the
QEMU part in brackets:

| Pass | Before (sequential) | After |
|---|---|---|
| `--all --jobs 10` | about 30 min | 4 min 27 s (QEMU 4 min 19 s) |
| `--all --cpus 4 --jobs 6` | about 30 min | 7 min 25 s (QEMU 6 min 55 s) |

Both passes: about an hour before, under 12 minutes after. The summed
scenario time of a parallel pass is 39 to 40 minutes. Earlier runs without
the container limits gave 5 min 21 s (`--jobs 8`) and 9 min 51 s
(`--cpus 4 --jobs 4`), all passing. A run with `--cpus 4 --jobs 8` while
the container was cut to 10 CPUs (32 vCPU threads on 10 CPUs) failed
`console.ps2` and `usb.hid_fault`: incident 0019 explains both and
records the fixes (a test expectation that raced QEMU's exit; one scenario
marked `serial`).

## Consequences

- The release check fits in a coffee break, and `develop` is tested on
  every commit without anyone starting it.
- A new host-side server for scenarios takes a per-run port and a
  placeholder; a new fixed port is only acceptable for something a guest
  test program has compiled in.
- One suite per network namespace remains, for the three compiled-in
  ports; Docker containers are separate namespaces.
- A scenario that depends on timing under load would now show it; it is a
  bug to fix (or a reason recorded here to run it alone), never a retry.
