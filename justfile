default: run

run *ARGS:
    cargo run -p rmut -- {{ARGS}}

test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --all --check

fmt:
    cargo fmt --all

e2e:
    cargo build
    python3 tests/e2e/run.py

check: test lint e2e

# crates.io: in dependency order, core first, the tui last
publish:
    cargo publish -p rmut-core
    cargo publish -p rmut-session
    cargo publish -p rmut-front
    cargo publish -p rmut
    cargo publish -p rmut-egui

# the window's desktop entry, Exec pinned to the installed binary:
# a GUI session rarely has ~/.cargo/bin on PATH
install-desktop:
    #!/usr/bin/env bash
    set -euo pipefail
    bin="$(command -v rmut-egui || true)"
    [ -n "$bin" ] || bin="$HOME/.cargo/bin/rmut-egui"
    [ -x "$bin" ] || { echo "no rmut-egui binary: cargo install rmut-egui" >&2; exit 1; }
    apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
    mkdir -p "$apps"
    sed "s|^Exec=rmut-egui$|Exec=$bin|; s|^TryExec=rmut-egui$|TryExec=$bin|" \
        crates/rmut-egui/dist/rmut-egui.desktop > "$apps/rmut-egui.desktop"
    # a rewritten file leaves the directory's mtime alone, and that mtime is
    # what invalidates GLib's app-info cache: without this a running
    # gnome-shell keeps serving the entry it read at login
    touch "$apps"
    command -v update-desktop-database >/dev/null && update-desktop-database "$apps" || true
    echo "installed $apps/rmut-egui.desktop -> $bin"
