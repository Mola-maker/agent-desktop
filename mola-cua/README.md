# mola-cua

Computer use for AI agents on Android, over `adb`, in the same model as agent-desktop: a snapshot of the screen as a tree, a ref for every element an agent can act on or read, one JSON envelope per command. Commands it does not know are handed to `agent-desktop` unchanged, so one entry point covers desktop and Android.

It lives in this fork as its own Cargo workspace and touches no upstream file, so the fork keeps syncing with vercel-labs/agent-desktop.

```sh
cd mola-cua && cargo build --release      # one binary, under 1 MB
mola-cua status                           # adb found? devices?
mola-cua health com.example.app --clear   # clear the crash log before launching
mola-cua install app-debug.apk
mola-cua launch com.example.app           # waits for `Status: ok`
mola-cua snapshot --app com.example.app -i --compact
mola-cua find --id login --first          # data.match.ref_id
mola-cua set-value '@s…:e2' 'ada'         # clears, types, reads the field back
mola-cua click '@s…:e4'
mola-cua wait --text 'Welcome' --timeout 10000
mola-cua health com.example.app           # running? crashed?
mola-cua snapshot --app Finder -i         # not an Android command: goes to agent-desktop
```

## How it differs from a script of adb calls

- **A ref is never a coordinate.** It stores what the element is (class, resource id, description, text, placeholder, the label a nameless row takes from the texts inside it, position in the tree). A list row is its label, not its slot: after a scroll the ref for "Alpha" does not tap whatever sits where Alpha was. Every action takes a fresh UI dump, finds that element again, and taps where it is now. Gone → `STALE_REF`. Several equal candidates and none in the same place → `AMBIGUOUS_TARGET`.
- **Errors say whether input was sent**, in agent-desktop-core's own terms: `not_delivered` (retry is safe), `delivery_uncertain` (the call that sends input did not come back), `delivered_unverified` (input went out and a later step failed). Retry only the first kind. The envelope is serialised through core's `Response` and `ErrorPayload`, so it parses with the same code.
- **`set-value` checks its own work.** It reads the field back and fails with `expected` and `found` when they differ. A password field is typed but reported `verified: false`.
- **Password content never leaves the device tree.** It is masked in snapshots, `find`, `get` and labels, is not searchable with `find --text`, and error messages name the adb command without its arguments, so typed text is not echoed back. The text does reach `adb` as an argument (`--stdin` only keeps it out of this command's own arguments).
- **Desktop commands are started without a shell.** On Windows the native agent-desktop binary inside the npm package is run directly, so `&`, `|` and `%VAR%` in an argument stay text.
- **More than one device is an error until you choose** (`--device <serial>` or `ANDROID_SERIAL`).
- **adb is found where Android Studio puts it** when it is not on PATH (`MOLA_CUA_ADB`, `ANDROID_HOME`, `ANDROID_SDK_ROOT`, the default SDK directories).

## Limits

- One `uiautomator dump` per observation and per action: expect about 1 to 3 s each on an emulator. No on-device helper is installed.
- `set-value` types printable ASCII only (`adb shell input text`).
- A WebView or a game surface exposes few nodes.
- State is kept in `~/.mola-cua/snapshots` (the last 40 snapshots); set `MOLA_CUA_HOME` to move it.
- Screenshots and snapshots can hold private content. Keep them local.

## What has been run on a device

On an Android 16 emulator (API 36.1, headless, 2026-11-13): `status`, `health --clear`, `launch` (activity resolved, `Status: ok`), `snapshot -i --compact`, `find`, `click` into a sub-screen, `wait`, a ref from the previous screen refused as `STALE_REF`, `press back`, `health`, `screenshot`, `close-app`, and `launch` of a package that is not installed (`APP_NOT_FOUND`). A dump took 2 to 7 s on that emulator, which was short of memory.

Not confirmed on a device: `set-value` putting text into a field. On that emulator typed text never appeared in the search field, with this tool or with plain `adb shell input text`, and the screen could not be inspected because the headless renderer produced a blank screenshot. The command reported the failure (`ACTION_FAILED`, expected and found) instead of claiming success. `install` has not been run with a real APK.

## Tests

`cargo test` runs against a scripted device (no adb, no emulator).
