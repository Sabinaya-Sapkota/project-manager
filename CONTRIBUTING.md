# Contributing

Thanks for your interest in improving Project Manager!

## Getting set up

1. Install [Rust](https://rustup.rs) and the [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your OS.
2. `cargo install tauri-cli --version "^2" --locked`
3. `cargo tauri dev` from the repository root.

## Project layout

| Path | What lives there |
| --- | --- |
| `src-tauri/src/main.rs` | Backend commands (SQLite via `rusqlite`) |
| `src-tauri/src/schema.sql` | Database schema |
| `ui/index.html` | The whole frontend (HTML, CSS, JS, no build step) |
| `demo/web-shim.js` | Browser implementation of the same commands, used by the web demo |

## Keeping the web demo in sync

The desktop app and the web demo share `ui/index.html`. Every backend command in `main.rs` has a matching implementation in `demo/web-shim.js`. If you add or change a command, update both.

## Pull requests

- Keep changes focused and describe how you tested them.
- Add a line to `CHANGELOG.md` for user-visible changes.
- Run `cargo check --manifest-path src-tauri/Cargo.toml` before opening a PR.
