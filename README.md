# agpeek

An App Group container inspector for iOS.

App Group containers are where your app and its extensions share state, and they are almost entirely opaque from the outside.
`agpeek` lists what is in one, decodes the binary formats inside it, and shows you what changed between two points in time.

## The core constraint

Everything about the design falls out of one question: **how do you get at the container?**
There are three mechanisms, with very different tradeoffs.

| Mechanism | Where | Live? | Requires app changes? |
|---|---|---|---|
| `simctl get_app_container <dev> <bundleid> groups` | Simulator | Yes (real path on disk) | No |
| `devicectl device copy from --domain-type group --domain-identifier group.x.y` | Device | No — pull-and-diff only | No, but build must be dev-signed |
| Embedded debug server in your app | Device | Yes | Yes (debug-only SPM dependency) |

Simulator and devicectl come first: zero integration cost, works on any project.
The embedded server is an optional "live mode" to add later.

The important honest bit: on a physical device you cannot watch the container with an inotify-style filesystem watcher.
Live device mode means polling — pull a snapshot every N seconds and diff — which is fine for a keyboard where writes are small and infrequent.

## Module layout

```
src/
├── main.rs          routing
├── cli.rs           clap definitions
├── discover.rs      find devices, apps, and their group IDs
├── source/          the "how do I read bytes" abstraction
│   ├── mod.rs       trait ContainerSource
│   ├── sim.rs       simctl-backed (direct fs access)
│   ├── device.rs    devicectl-backed (pull to temp dir)
│   └── live.rs      HTTP/Bonjour client for embedded server
├── snapshot.rs      Snapshot { files, hashes, taken_at }
├── diff.rs          two Snapshots → ChangeSet
├── decode/          make bytes human-readable
│   ├── plist.rs     binary plist → JSON
│   ├── keyedarchive.rs  NSKeyedArchiver unwrapping
│   └── sqlite.rs    Core Data / SQLite table dump
└── ui/              tree render, diff render, tui watch mode
```

The `ContainerSource` trait is the whole architecture.
It needs roughly three things: list entries under a path, read a file, and report whether it supports native watching.
Everything above it — snapshot, diff, decode, render — is source-agnostic.
That is what lets simulator support ship in a weekend and device support land later without a rewrite.

## Where the real value is: `decode/`

Listing files is table stakes.
The reason this tool is worth using is that App Group contents are mostly *opaque binary*.

- **`Library/Preferences/group.com.you.app.plist`** — this is your shared `UserDefaults` suite.
  It is a binary plist.
  Decoding it is the single highest-value feature, because right now the only way to see it is `po` in a debugger with the extension attached.
- **NSKeyedArchiver blobs** — anything you stored as a `Data` from an archived object.
  These are plists containing an object graph with `$objects`/`$top` indirection.
  Unwrapping them into readable JSON is a genuinely satisfying parsing problem.
- **SQLite / Core Data stores** — if the app caches anything structured.

Ship with `--raw` as an escape hatch, and treat every decoder as best-effort: try to decode, fall back to a hexdump, never crash on an unknown format.

## Command surface

```
agpeek devices                       # what's connected
agpeek groups <bundle-id>            # read entitlements, list group IDs
agpeek ls <group-id> [path]          # tree view, sizes, mtimes
agpeek cat <group-id> <path>         # auto-decoded, --raw to bypass
agpeek defaults <group-id>           # the shared UserDefaults suite, pretty-printed
agpeek watch <group-id>              # TUI, live diff
agpeek snapshot <group-id> -o a.json
agpeek diff a.json b.json
```

`snapshot` + `diff` is the sleeper feature.
Snapshot before typing, snapshot after, diff.
That tells you exactly what the keyboard wrote — which is the question you are really asking when you reach for a tool like this.

## Build order

1. `ls` + `cat` on the simulator only.
   Hardcode the source, no trait yet.
2. Extract `ContainerSource` once you add devicectl.
   The second implementation is what tells you the right trait shape, not the first.
3. Binary plist decoding + `defaults`.
   This is the moment it becomes something you would actually reach for.
4. `snapshot` / `diff`.
5. `watch` TUI (ratatui) on top of repeated diffs.
6. Optional: the embedded debug server for true live device mode.

Stopping after step 3 is a legitimate ending.
That is already a useful tool, and shipping something small and finished beats a half-built one.

## Design warning

**Treat everything you read as untrusted input.**
You are parsing binary formats that a buggy app wrote.
Fuzz the plist decoder, or at minimum make sure malformed input returns an error rather than panicking.
Otherwise your debugging tool becomes another thing to debug.
