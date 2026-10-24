# ADR 0048: HTTP redirects, and a public update source

- **Date:** 2026-10-24
- **Status:** Accepted for the HTTP client (decisions 1 and 2); proposed,
  waiting for the owner, for where the public source lives (decision 3).
  Extends ADR 0026 (HTTPS) and ADR 0029 (update sources).

## Context

Machines update from sources the owner registers (ADR 0029); so far every
source has been a server on the owner's LAN. A machine outside it needs a
public source. The two places the project already has:

- **GitHub Releases** of `AriCodeLabs/AriCodeOS`, where each release
  already carries its USB image. A stable URL for the newest release's
  files exists, `https://github.com/AriCodeLabs/AriCodeOS/releases/latest/download/NAME`,
  but it answers with redirects: `latest` to the tag
  (`/releases/download/v0.13.0/NAME`), the tag to the asset host
  (`release-assets.githubusercontent.com`, a signed URL of about 900
  bytes). Measured on 2026-10-24: two redirects, and 10.6 KiB of response
  headers on github.com, one line (the Content-Security-Policy) of 9,243
  bytes.
- **www.aricodeos.org**, the owner's own server (nginx), serving files
  directly.

The HTTP client treated any status but 200 as a failure and held 8 KiB of
response headers, so from the reference laptop `http` on a release asset
answered "response headers too large", although the TLS handshake with
github.com and with the asset host succeeded (their chains end at roots
the client embeds: Sectigo through USERTrust, and ISRG Root X1 through the
new Let's Encrypt `Root YR`).

## Decision

1. **The HTTPS client follows redirects** (`aricode_https::get_with`, so
   the updater and the shell's `http`):
   - statuses 301, 302, 303, 307 and 308 with a `Location`; each hop is a
     `GET` on a new connection, within the same budget as the whole
     download;
   - at most `MAX_REDIRECTS` = 5 (GitHub needs two); one more is
     `TooManyRedirects`;
   - never from `https://` to `http://` (`InsecureRedirect`); `http://` to
     `https://` is fine, so a LAN source may move to TLS;
   - the `Location` is printable ASCII (a server percent-encodes the
     rest), resolved against the request (absolute, scheme-relative,
     absolute-path, query-only or relative; dot segments removed from the
     path, never the query; fragment dropped; RFC 3986) and must be an
     `http(s)` URL of at most `MAX_LOCATION` = 2048 bytes whose host is a
     host name or an IPv4 address (letters, digits, `-`, `_`, `.`; no
     `user@`) and whose port is not 0; otherwise `BadRedirect`. Two
     different `Location` headers make the response malformed;
   - a redirect's body never reaches the caller's sink.
   `aricode_http::fetch_with_head`, used by the plain `sys::http` client
   without `alloc`, keeps returning a redirect as its status.

2. **Larger response headers over HTTPS.** The header buffer is sized by
   the caller: the HTTPS client keeps `MAX_HEADERS_LARGE` = 16 KiB on the
   heap (the stack variant keeps 8 KiB), and accepts twice its buffer in
   header bytes per fetch as before (incident 0009).

3. **The public source (proposed).** A stable release publishes its update
   source, the output of `cargo xtask publish-update --version N
   --signing-key ~/.config/aricodeos/signing-key` (`index`, `index.sig`,
   `aricodeos-N.img`, signed with the release key), and machines register
   one URL. The index names its files relative to that URL, so either
   place works without code changes:
   - GitHub Releases: the files attached to each release, the source URL
     `https://github.com/AriCodeLabs/AriCodeOS/releases/latest/download/`
     (follows the newest non-prerelease release; publishing is `gh release
     upload`, which the owner approves per release);
   - the owner's server: a directory such as
     `https://www.aricodeos.org/updates/stable/`, updated by the owner.
   Release sticks would then carry that URL (`usb-image --update-source`),
   so a fresh machine finds updates without configuration; the owner can
   still remove it (`update remove`), and LAN or development sources keep
   working beside it.

## Consequences

- Trust does not move: the index is signed with the release key and never
  replayed (serials), the image and packages match its SHA-256 and carry
  their own signatures. A redirect, even to another host, cannot make the
  updater accept anything it would not accept from the first URL; it can
  only make the download fail. Binding an index to the source that serves
  it is still open (ADR 0029).
- With `latest`, a release published between a machine's index fetch and
  its image fetch moves the URL: the image named by the older index is not
  in the new release (HTTP 404), the run fails and the next one sees the
  new index.
- The plain `sys::http` client does not follow redirects; nothing in the
  system uses it for update sources.
- A source can make a machine send a few blind `GET`s elsewhere, an
  internal address included (at most six requests per download, within
  its budget, responses never returned to the source). That is what any
  redirect-following client allows; the updater trusts nothing it gets
  that way.
- The independent audit (no high findings) found a panic on a non-ASCII
  `Location` (a slice inside a character), now refused before any slice
  and covered by a random test, and the query, scheme and host rules
  above. A request now holds a path as long as `MAX_LOCATION` (the
  reference laptop found the 512-byte limit with GitHub's signed asset
  URL, about 900 bytes).

## Tests

- Host: redirect parsing, the buffer sizes (a 9,243-byte line needs the
  16 KiB buffer), location resolution with dot segments, the policy
  (GitHub's two hops, relative hops, the downgrade refusal, the hop limit,
  bad targets) over scripted exchanges.
- `update.redirected_source` (full): every file of a source takes two
  redirects (one with a 9,243-byte header line, one relative); `update`
  and `upgrade` work through them, a redirect body never shows, and the
  shell's `http` stops a redirect loop after five and refuses `ftp://`.
- On the reference laptop: `http` on a GitHub release asset follows both
  redirects over TLS to the asset host.
