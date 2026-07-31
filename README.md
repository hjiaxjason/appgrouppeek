# agpeek

An App Group container inspector for iOS.

App Group containers are where your app and its extensions share state, and they are almost entirely opaque from the outside.
`agpeek` lists what is in one, decodes the binary formats inside it, and shows you what changed between two points in time.

The question it exists to answer: *what did my keyboard extension actually write when I typed that?*

## Install

Requires a recent Rust toolchain and Xcode command-line tools.

```
cargo install --path .
```

Or run it out of the build directory without installing:

```
cargo build --release
./target/release/agpeek devices
```

Simulator only for now. See [Not built yet](#not-built-yet).

## Usage

```
agpeek devices                          # simulators that can be inspected
agpeek groups <bundle-id>               # App Groups an installed app declares
agpeek ls <group-id> [path]             # file tree, sizes, mtimes
agpeek cat <group-id> <path>            # a file, decoded where possible
agpeek defaults <group-id>              # the shared UserDefaults suite
agpeek snapshot <group-id> -o a.json    # record the container
agpeek diff a.json b.json               # what changed between two snapshots
```

Global flags, valid on any subcommand:

| Flag | Effect |
|---|---|
| `--device <UDID\|NAME>` | Which simulator. Defaults to the only booted one. |
| `--json` | Machine-readable output instead of a table. |
| `--no-color` | Disable colour. `NO_COLOR` and non-terminal output are already honoured. |

### Addressing a container

`<group-id>` is the App Group identifier, e.g. `group.app.natively.shared`.
You do not need a bundle ID — containers are found by scanning their own metadata.

Group identifiers are **not** unique: `systemgroup.com.apple.accessorysetupkit` exists under both the `AppGroup` and `SystemGroup` roots on a stock simulator, with different contents.
When an identifier is ambiguous, `agpeek` refuses to guess and lists the candidates:

```
$ agpeek ls systemgroup.com.apple.accessorysetupkit
error: `systemgroup.com.apple.accessorysetupkit` matches 2 containers — pass the UUID instead:
  36E1E225-01C1-4AE6-869F-980161FF2F03 (app)
  8543D632-869E-460E-BBF7-17067DD6DB2C (system)
```

Any command that takes a `<group-id>` will also take a container UUID, which is how you disambiguate.

## Try it without installing anything

Every simulator already has dozens of App Group containers, so you can exercise the whole tool before wiring up your own app.
With a simulator booted:

```
agpeek devices
agpeek groups com.apple.news          # five real groups
agpeek ls group.com.apple.weather
agpeek defaults group.com.apple.weather
```

That last one prints the decoded binary plist:

```
WeatherProviderName: ""
deviceUUID: "DEVICE_85B73D09-CF9C-407B-BE32-0C167C562D6E"
notificationsDataFormatVersion: 2.0
verificationData: "35551970a6fc42f12918dce6ee44963529634050cea7cd219c7555655091445b"
```

Strings are quoted and integral reals print as `2.0` rather than `2`, so an empty string, a numeric-looking string, and a real that happens to be whole all stay distinguishable — the distinctions that matter when you are checking what an app really wrote.

## The workflow this was built for

Snapshot, exercise the app, snapshot again, diff:

```
agpeek snapshot group.app.natively.shared -o before.json
# ... type on the keyboard ...
agpeek snapshot group.app.natively.shared -o after.json
agpeek diff before.json after.json
```

```
~ Library/Preferences/group.app.natively.shared.plist
  ~ keyboard_diagnostics.net_dns: "ok" → "slow"
  ~ usageCount: 3 → 4
```

**Key-level diffing is the point.** Natively's container holds exactly one file, so a file-level diff would report "1 file modified" every single time and answer nothing.
When both sides of a changed file decode as plists, `agpeek` descends into their keys and reports the change by dotted path, nested dictionaries included.
When it cannot descend — the file is not a plist, or was too large to store inline — it says only what it actually knows rather than inventing key detail.

### Seeing a diff right now

If your own app is not installed yet, you can prove the pipeline in thirty seconds against any container. This is reversible; it adds one file and removes it.

```
C=$(agpeek ls group.com.apple.weather --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["root"])')

cat > "$C/agpeek-demo.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>usageCount</key><integer>3</integer>
<key>diagnostics</key><dict><key>net</key><string>ok</string></dict>
</dict></plist>
EOF

agpeek snapshot group.com.apple.weather -o /tmp/before.json

plutil -replace usageCount -integer 4 "$C/agpeek-demo.plist"
plutil -replace diagnostics.net -string slow "$C/agpeek-demo.plist"

agpeek snapshot group.com.apple.weather -o /tmp/after.json
agpeek diff /tmp/before.json /tmp/after.json

rm "$C/agpeek-demo.plist"    # clean up
```

```
~ agpeek-demo.plist (297 → 315 bytes)
  ~ diagnostics.net: "ok" → "slow"
  ~ usageCount: 3 → 4
```

## Reading files

`cat` sniffs content by magic bytes, never by filename — a `.plist` extension on a JPEG proves nothing about what an app wrote.

| Content | Output |
|---|---|
| Binary or XML plist | decoded, keys sorted |
| SQLite database | recognised, named, hexdump (not decoded) |
| PNG | recognised with pixel dimensions |
| UTF-8 text | verbatim |
| Anything else | hexdump |

Decoding is best-effort and **never fails the command**: content that cannot be parsed falls back to a hexdump with a note saying why.
`--raw` skips decoding entirely, and `--limit <BYTES>` caps hexdump output (`0` for no cap).

```
$ agpeek cat group.com.apple.weather Library/Preferences/group.com.apple.weather.plist --raw
00000000  62 70 6c 69 73 74 30 30  d4 01 02 03 04 05 06 07  |bplist00........|
00000010  08 5a 64 65 76 69 63 65  55 55 49 44 5f 10 13 57  |.ZdeviceUUID_..W|
```

## Snapshots

A snapshot is JSON, sorted by path and stable across runs, so two of them compare cleanly and are reviewable by eye.
Each file carries its size, mtime, and SHA-256; plists under 256 KB also store their decoded content inline, which is what makes a key-level diff possible after the container has moved on.

`agpeek snapshot` writes to stdout when `-o` is omitted, and its confirmation line goes to stderr, so both forms are scriptable.

`diff` refuses two snapshots of different groups rather than reporting every file as both added and removed.

## Design notes

**Discovery is separated from reading.**
`discover.rs` shells out to `simctl` and yields a container root path; everything above it operates on a plain directory tree.
That is what keeps the walk, decode, snapshot, and diff layers testable without a simulator booted.

**Everything read is untrusted input.**
These are binary formats written by an app that may be buggy.
The plist decoder is tested against truncation at every offset, a flipped bit at every offset, and a spread of arbitrary inputs, each asserting an error rather than a panic.
Conversion is depth-bounded so runaway nesting cannot exhaust the stack.
A debugging tool that needs debugging is worse than useless.

**Ambiguity is an error, never a guess.**
Two booted simulators, a device name matching two runtimes, a group identifier under both container roots — each of these stops and lists the candidates.
Picking one arbitrarily produces a confusingly empty result rather than an obvious failure.

## Not built yet

| | Status |
|---|---|
| Physical device support (`devicectl`) | Not started. Would be pull-and-diff only; no live watching without an embedded server. |
| `watch` TUI | Not started. Only genuinely useful once device polling exists — on the simulator a container is a real directory you can simply re-read. |
| `NSKeyedArchiver` unwrapping | Not started. Needed for archived `Data` values; the `$objects`/`$top` graph walk is written by hand. |
| SQLite / Core Data decoding | Not started. Recognised and reported, not parsed. |
| `--container-path` | Not started. Would let `ls`/`cat`/`snapshot` run against a directory, making them testable without a simulator the way `diff` already is. |

The `ContainerSource` trait the original design called for does not exist yet, deliberately.
The second implementation is what reveals the right trait shape; extracting it before `devicectl` exists would be guessing.

## Development

```
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Plist fixtures live in `tests/fixtures/` as XML so they are reviewable in a diff; tests re-encode them to binary, which is the form actually found in containers.
