default:
    @just --list

# Enter the Nix development shell.
develop:
    nix develop

# Check the Nix flake.
check:
    nix flake check

# Update flake inputs.
update:
    nix flake update

# Show flake outputs.
show:
    nix flake show

# Format Nix files.
fmt:
    nix fmt

# Build the CLI.
build:
    cargo build --release

# Grant CAP_NET_RAW so raw Ethernet frames can be sent without sudo.
setcap: build
    sudo setcap cap_net_raw+ep target/release/auto-ip

# Remove CAP_NET_RAW from the release binary.
uncap:
    sudo setcap -r target/release/auto-ip

# Send frames with the release binary; run `just setcap` first.
send *ARGS:
    target/release/auto-ip {{ARGS}}

# Run the CLI; pass arguments directly, e.g. `just run -i eno1`.
run *ARGS:
    cargo run --release -- {{ARGS}}

# Type-check the CLI.
cargo-check:
    cargo check

# Run unit tests.
test:
    cargo test

# Format Rust sources.
rust-fmt:
    cargo fmt

# Lint Rust sources.
clippy:
    cargo clippy -- -D warnings
