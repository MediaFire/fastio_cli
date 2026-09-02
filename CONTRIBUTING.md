# Contributing to Fast.io CLI

Thank you for your interest in contributing to the Fast.io CLI!

## Getting Started

1. Fork the repository
2. Clone your fork: `git clone https://github.com/YOUR_USERNAME/fastio_cli.git`
3. Create a branch: `git checkout -b my-feature`
4. Make your changes
5. Submit a pull request

## Development Setup

```bash
# Install Rust (stable)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build
cargo build

# Run tests
cargo test

# Check formatting
cargo fmt --check

# Run linter
cargo clippy -- -W clippy::pedantic
```

## Before Submitting a PR

Run the full check cycle:

```bash
cargo fmt
cargo clippy -- -W clippy::pedantic
cargo check
cargo test
cargo build --release
```

All checks must pass with zero warnings.

## Code Standards

- No `unwrap()` or `expect()` in production code — handle all errors
- Use `thiserror` for error enums, `anyhow` with `.context()` at command boundaries
- All public items must have doc comments
- Use `secrecy::SecretString` for tokens and passwords in memory
- URL path parameters must use `urlencoding::encode()`
- Errors go to stderr, structured output to stdout

## Reporting Issues

- **Bugs**: Open a GitHub issue with steps to reproduce
- **Security vulnerabilities**: See [SECURITY.md](SECURITY.md)
- **Feature requests**: Open a GitHub issue with your use case

## License

By contributing, you agree that your contributions will be licensed under the Apache License 2.0.
