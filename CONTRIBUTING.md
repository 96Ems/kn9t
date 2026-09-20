# Contributing to kn9t

## Quick start

```bash
git clone https://github.com/96Ems/kn9t
cd kn9t
cargo build --release
cargo test --workspace

# Build the tools plugin
cd plugins/kn9t-tools && cargo build --release
```

## Before submitting a PR

```bash
# Run all CI checks locally
bash scripts/check-ci.sh

# If you edited schema/*.json, regenerate
cargo run -p xtask -- generate
```

## Code guidelines

Read [`AGENTS.md`](AGENTS.md) for architecture and coding conventions.

Key points:
- **Schema-first:** Edit `schema/*.json`, run `xtask generate`, commit both
- **No async:** OS threads only, no tokio
- **TUI talks HTTP:** If TUI needs data, add a server endpoint
- **Tests required:** A change is done when its test passes

## Reporting bugs

Open an issue with:
1. What you expected
2. What happened
3. Steps to reproduce
4. kn9t version (`kn9t --version`)

## Proposing features

Open an issue describing:
1. The problem you're solving
2. Your proposed solution
3. Alternatives you considered

For significant changes, discuss in an issue before implementing.

## License

By contributing, you agree that your contributions will be licensed under MIT.
