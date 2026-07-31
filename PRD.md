# PRD — `agpeek`

**Status:** approved
**Author:** Jason
**Last updated:** 2026-07-31

---

## 1. Problem

iOS App Group containers are where an app and its extensions share state, and they are effectively a black box from the outside.

When debugging the Natively keyboard, the only way to see what the extension wrote into the shared container is to attach a debugger to the extension and `po` the values.
That is slow, it requires the extension to be running, and it cannot answer the question that actually comes up in practice: *what changed in the container when I typed that?*

The contents are also mostly opaque binary.
The shared `UserDefaults` suite is a binary plist; anything stored as archived `Data` is an `NSKeyedArchiver` object graph.
Neither is readable with `cat`.

## 2. Goals

- Read an App Group container's contents from the command line, with no debugger and no app modification.
- Decode the binary formats found there into human-readable output, best-effort, never crashing on malformed input.
- Answer "what did the app write between these two moments?" precisely enough to be useful when the container holds a single file.
- Work on any project, not just Natively — no SDK, no integration step, no code changes in the target app.

## 3. Non-goals

- Writing to or mutating container contents. `agpeek` is read-only.
- Physical-device support in v1 (see §8).
- A live filesystem watcher. On a physical device this is not possible without an embedded server; polling is the only option, and it is deferred.
- Being a general plist editor or a Core Data browser.

## 4. Users and use cases

Primary user is the developer of an iOS app with extensions — initially Jason, debugging Natively.

| # | Use case | Command |
|---|---|---|
| U1 | "Which simulators can I inspect?" | `agpeek devices` |
| U2 | "What groups does this app declare?" | `agpeek groups app.natively` |
| U3 | "What's in the container?" | `agpeek ls group.app.natively.shared` |
| U4 | "What's in this file?" | `agpeek cat <group> <path>` |
| U5 | "What's in the shared UserDefaults suite?" | `agpeek defaults <group>` |
| U6 | "What did the keyboard write when I typed?" | `agpeek snapshot` ×2 + `agpeek diff` |

U6 is the reason the tool exists.
U5 is the reason it becomes usable day to day.

## 5. Background and constraints

Two findings from exploring the machine materially shape this PRD.

### 5.1 There is no parser groundwork to reuse

The originating design note assumed the binary-format work would build on `make-aid`'s `parser.rs`.
That file is 38 lines of line-oriented regex over C `#include` directives, it does not compile, and it is not in the crate's module graph (`main.rs` declares no `mod`).
`make-aid` overall is a ~263-line non-compiling skeleton with three empty source files, no tests, and no error handling.

What carries over is conventions only: `edition = "2024"`, clap derive with `///` doc comments as help text, a commented dependency list, and `//!`/`///` doc discipline.
Error handling, testing, and all parsing are new work.

### 5.2 The real target container is a single file

Natively (`app.natively`, group `group.app.natively.shared`) writes exactly one thing: `Library/Preferences/group.app.natively.shared.plist`.
It is a binary plist containing `natively_user_id`, `usageCount`, `usageMonth`, `tonePreference`, `rewriteContextPreference`, `chineseModeEnabled`, and `keyboard_diagnostics` (a nested dict of strings).
There is no Core Data, no SQLite, no `NSKeyedArchiver`, no JSON.
The only other container file is an optional, hand-placed `keyboard-calibration-reference.png` at the container root.

**Consequence, and the single most important requirement in this document:** a file-level diff of Natively's container would report "1 file modified" and nothing else, every time.
Key-level plist diffing is a hard requirement of Slice 4, not a stretch goal.

### 5.3 Verified platform mechanics

Confirmed by running against the local toolchain (`xcrun` 72, devicectl 518.31, cargo 1.97.0, one booted sim `iPhone 17` / `B943F0CB-…`).

- `xcrun simctl get_app_container <device> <bundle-id> groups` prints **tab-separated** `group-id<TAB>absolute-path`, one line per group.
- The same command takes a specific group identifier as its last argument to resolve a single path.
- **Group IDs do not reliably start with `group.`** — real examples on this machine include `systemgroup.com.apple.accessorysetupkit` and a bare `com.apple.CoreODI`. Nothing may validate on that prefix.
- A group container can be resolved **without any bundle ID** by scanning `~/Library/Developer/CoreSimulator/Devices/<udid>/data/Containers/Shared/AppGroup/*/.com.apple.mobile_container_manager.metadata.plist` for the `MCMMetadataIdentifier` key. This is what makes `agpeek ls <group-id>` work standalone, and it is the primary resolution path.
- Natively is **not currently installed on any simulator**. Installing it requires `/Users/hjason/Natively/scripts/verify-keyboard.sh --setup`, after which the keyboard and Full Access must be enabled by hand in Settings — `simctl` cannot automate that.
- Apple's own group containers are usable as fixtures **today**: `group.com.apple.weather` (a 266-byte `bplist00`, four keys), `group.com.apple.mail`, `group.is.workflow.my.app`. Slices 1–3 are fully verifiable before Natively is ever installed.

## 6. Design decisions

| Decision | Choice | Rationale |
|---|---|---|
| Binary plist decoding | `plist` crate | Battle-tested against malformed input, returns `Result`. The hand-written parsing problem is preserved in the `NSKeyedArchiver` graph walker, which the crate does not do for you. |
| Scope | Through `snapshot`/`diff` | `defaults` makes it usable; `diff` answers U6. |
| Device support | Deferred | Simulator-only for v1. |
| `ContainerSource` trait | **Not** extracted in v1 | The second implementation reveals the right trait shape. Extracting it before `device.rs` exists is guessing. |

### 6.1 Architecture: separate discovery from reading

Discovery (`discover.rs`) shells out to `simctl` and yields a container root `PathBuf`.
Reading (`source/sim.rs`) takes a root path and does plain filesystem work, knowing nothing about simulators.

Everything above the read layer — snapshot, diff, decode, render — operates on a directory tree.
So `ls`, `cat`, `defaults`, `snapshot`, and `diff` are all integration-testable against a fixture directory, with no simulator booted and no `simctl` on the path.
Only `devices` and `groups` need a real simulator.

### 6.2 Cross-cutting requirements

- **R1 — Never panic on container contents.** Every decoder is best-effort: decode, or fall back to a hexdump with a note. Malformed input returns an error; it does not abort the command and does not unwind. This is the tool's core reliability promise — a debugging tool that needs debugging is worthless.
- **R2 — `--raw` everywhere.** Any decoded output can be bypassed.
- **R3 — `--json` everywhere.** Every command emits machine-readable output for scripting.
- **R4 — Errors are actionable.** `anyhow` with `.context()`; `main()` prints the full chain to stderr and exits non-zero.
- **R5 — Content sniffing, not extensions.** Dispatch on magic bytes, never on filename alone.

---

## 7. Vertical slices

Each slice is independently shippable and independently useful.
Slices land in order; the tool is a legitimate finished product after Slice 3.

### Slice 1 — `devices`, `groups`

**User story:** as a developer, I can see which simulators are inspectable and which App Groups an app declares.

The thinnest end-to-end cut — subprocess → parse → render.
Its real purpose is to stand up the crate skeleton and the conventions every later slice inherits.

Scope: `Cargo.toml`, `src/main.rs`, `src/cli.rs`, `src/discover.rs`, `src/ui/mod.rs`.

- clap definitions live in `cli.rs`, not `main.rs`. (`make-aid` documented this split and never did it.)
- Global flags declared once with `#[arg(global = true)]`: `--device <udid|name>`, `--json`, `--no-color`.
- Device selection: default to the single booted device; if zero or more than one is booted, error and name the candidates. Parse `simctl list devices booted -j`.
- A `Command` runner helper in `discover.rs`: check `status.success()` before trusting stdout, `from_utf8_lossy` on both streams, `.context()` on spawn failure.

**Acceptance criteria**
- `agpeek devices` lists the booted iPhone 17.
- `agpeek groups com.apple.news` prints its five group IDs and paths, parsed from tab-separated output.
- Both support `--json`.
- A bundle ID with no groups, and one that is not installed, each produce a clear error rather than an empty success.

### Slice 2 — `ls`

**User story:** as a developer, I can see the file tree of a container addressed by group ID alone.

Scope: `src/source/sim.rs`, `src/ui/tree.rs`, extends `discover.rs`.

- `discover::resolve_group(device, group_id) -> PathBuf` via the metadata-plist scan (§5.3), so no bundle ID is needed. Fall back to `get_app_container` when one is supplied.
- `source/sim.rs` exposes `Container { root: PathBuf }` with list-entries and read-file over `walkdir`.
- Tree render with sizes and mtimes; `--depth`; `-a` for dotfiles (the container metadata plist is a dotfile and is worth seeing).

**Acceptance criteria**
- `agpeek ls group.com.apple.weather` renders the tree including `Library/Preferences/group.com.apple.weather.plist` with plausible size and mtime.
- A group ID without the `group.` prefix resolves correctly.
- Symlinks are not followed; unreadable entries are reported inline without aborting the walk.

### Slice 3 — `cat`, `defaults` *(the payoff)*

**User story:** as a developer, I can read the shared `UserDefaults` suite without attaching a debugger.

Scope: `src/decode/mod.rs`, `src/decode/plist.rs`, `src/ui/value.rs`.

- `decode::render(bytes, path) -> Rendered`, sniffing per R5: `bplist00` → binary plist; leading `<?xml` → XML plist; `SQLite format 3\0` → recognised-but-unsupported with a pointer to `--raw`; valid UTF-8 → text; otherwise hexdump.
- `decode/plist.rs` wraps the `plist` crate behind one narrow function so the implementation stays swappable, converting to `serde_json::Value` for rendering and `--json`. Handle types JSON lacks explicitly: `Data` (base64 + byte count), `Date` (RFC 3339), `Uid`, and integers outside the `f64`-safe range.
- `defaults <group-id>` is `cat` aimed at `Library/Preferences/<group-id>.plist`, printed as a sorted key/value table. It must say plainly when that file does not exist yet — for a freshly installed app, it will not.

Testing starts here; there is nothing to inherit.
- Commit fixtures as **XML** plists, converted to binary at test time with `plutil -convert binary1`, rather than committing Apple's simulator artifacts (which carry device UUIDs and verification blobs).
- A malformed-input suite truncates each fixture at a range of offsets and asserts `Err`, never a panic — R1, discharged concretely.

**Acceptance criteria**
- `agpeek defaults group.com.apple.weather` prints its four real keys.
- `agpeek cat --raw` on the same file hexdumps it.
- Nested dicts render readably (validated later against `keyboard_diagnostics`).
- Truncating that plist to 20 bytes yields a clean error, not a panic.
- A PNG and a text file in the same container each render sensibly.

### Slice 4 — `snapshot`, `diff` *(the Natively workflow)*

**User story:** as a developer, I can snapshot before and after typing and see exactly which keys the keyboard changed.

Scope: `src/snapshot.rs`, `src/diff.rs`, `src/ui/diff.rs`.

- `Snapshot { version, group_id, device_udid, taken_at, files }`, `FileEntry { path, size, mtime, sha256 }`, JSON, **sorted by path** so diffs are stable and files are eyeball-reviewable.
- `version` present from the first commit so the format can evolve without breaking saved snapshots.
- `diff.rs` yields `ChangeSet { added, removed, modified }` by path and hash.
- **Key-level plist diff is required** (§5.2). When both sides of a modified file decode as plists, descend and report added/removed/changed keys with old and new values, including into nested dicts like `keyboard_diagnostics`. Fall back to file-level reporting when either side does not decode.
- To diff without the container still present, snapshots store decoded plist content inline for small plists alongside the hash, capped at a few hundred KB, hash-only above that.
- No redaction by default (`natively_user_id` is an anonymous UUID), but keep the JSON shape amenable to `--redact <key>` later, since snapshots get pasted into issues.

**Acceptance criteria**
- Snapshot → type on the keyboard → snapshot → `agpeek diff before.json after.json` reports `usageCount 3 → 4`.
- A change confined to a nested dict is reported at the nested key, not as "file modified".
- Diffing two snapshots of different group IDs is refused with a clear error.
- Snapshot JSON is stable across runs when nothing changed.

### Slice 5 *(optional)* — NSKeyedArchiver unwrapping

Not needed for Natively, which writes no keyed archives.
Worth building for general tool value, and it is the hand-written parsing problem that survived the decision to use the `plist` crate.

Scope: `src/decode/keyedarchive.rs`.

- Detect via `$archiver` / `$objects` / `$top` on an already-decoded plist, then resolve `CF$UID` indirection into a plain object graph.
- Because this is untrusted input from a possibly-buggy app: detect reference cycles, cap recursion depth, bounds-check every UID against `$objects`. Return an error rather than recursing to a stack overflow.
- Fixtures generated locally with a small Swift `NSKeyedArchiver` snippet; several Apple simulator group containers also contain real archives.

**Acceptance criteria**
- A round-tripped archive unwraps to readable JSON.
- A hand-corrupted archive with a self-referential UID returns an error and does not overflow the stack.

---

## 8. Out of scope for v1

devicectl / physical-device support, the `ContainerSource` trait, the `watch` TUI, the embedded debug server, and SQLite/Core Data decoding.

`watch` is only genuinely useful once device polling exists — on the simulator the container is a real directory that can simply be re-read.
The SQLite sniff in Slice 3 exists solely so the tool says something useful instead of hexdumping a database.

## 9. Success criteria

1. `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` clean; `cargo test` green. Lints and flaky tests get fixed when encountered, not routed around.
2. Slices 1–3 verify against Apple's group containers on the booted iPhone 17, with no Natively build required.
3. For Slice 4: install Natively via `/Users/hjason/Natively/scripts/verify-keyboard.sh --setup`, enable the keyboard and Full Access by hand, confirm `group.app.natively.shared.plist` appears, then run the snapshot → type → snapshot → diff loop.
4. No code path panics on container contents; the malformed-input suite covers every decoder shipped.
5. The tool answers U6 in one command, from a cold terminal, in under ten seconds.

## 10. Risks and open questions

| Risk | Mitigation |
|---|---|
| Natively's container never materialises on the simulator because Full Access can't be enabled programmatically | Slices 1–3 do not depend on it; Slice 4 can be validated first against a synthetic fixture directory, then confirmed on the real container. |
| `simctl` output format changes between Xcode versions | Parsing is confined to `discover.rs`; prefer `-j` JSON output where offered. |
| Inline plist content in snapshots makes them large or leaks values into pasted issues | Size cap with hash-only fallback; `--redact` reserved in the format. |
| The `plist` crate proves limiting | It sits behind a single function; swapping in a hand-rolled parser touches no callers. |

**Open question, deferred:** whether `watch` should ever exist for the simulator, or only ship alongside device support. Revisit after Slice 4 is in daily use.
