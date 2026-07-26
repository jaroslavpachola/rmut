default: run

run *ARGS:
    cargo run -p rmut-tui -- {{ARGS}}

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

# crates.io: core must land first, the tui depends on it
publish:
    cargo publish -p rmut-core
    cargo publish -p rmut-tui
