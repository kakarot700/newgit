# Contributing to NewGit

Thanks for your interest. Start with the detailed [contributor guide](docs/CONTRIBUTING.md), which explains the code map, design contract, testing expectations, and documentation rules. Please also read the [Code of Conduct](CODE_OF_CONDUCT.md).

Before opening a pull request, run these checks from the repository root:

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo test --release --locked
```

Include the motivation, behavior change, relevant tests, and any compatibility or security implications in your pull-request description. Do not include credentials, private repository data, or real secrets in issues, commits, fixtures, or logs. For suspected vulnerabilities, follow [SECURITY.md](SECURITY.md) instead of opening a public issue with exploit details.
