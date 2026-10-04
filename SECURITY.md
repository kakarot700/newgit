# Security policy

NewGit is a **Production Candidate**, not a security-certified product. Review the [security model](SECURITY_MODEL.md), [threat model](THREAT_MODEL.md), [known limitations](KNOWN_LIMITATIONS.md), and [deployment guide](docs/DEPLOYMENT.md) before exposing a repository or remote service.

## Reporting a vulnerability

Please use this repository's **GitHub Security → Report a vulnerability** private reporting feature to contact the maintainers. Do not publish exploit details, credentials, or a proof of concept in a public issue or pull request. If private reporting is unavailable, contact the repository owner through GitHub's private contact channel and include enough detail to reproduce the issue safely.

There is currently no published response-time guarantee, bug-bounty program, or supported-version policy. Reports will be triaged as maintainer capacity allows. Please include the affected commit or release, environment, impact, and a minimal reproduction where safe.

## Deployment notes

The built-in remote protocol v1 uses plain HTTP. Do not expose it directly to an untrusted network; use a TLS-terminating reverse proxy or a trusted tunnel as described in `docs/DEPLOYMENT.md`. NewGit does not implicitly run repository content; `evidence record` executes only the command explicitly supplied by its caller.
