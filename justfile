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
