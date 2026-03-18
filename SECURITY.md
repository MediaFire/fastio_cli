# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in the Fast.io CLI, please report it responsibly.

**Do NOT open a public GitHub issue for security vulnerabilities.**

Instead, please email **security@fast.io** with:

- A description of the vulnerability
- Steps to reproduce
- The potential impact
- Any suggested fixes (optional)

We will acknowledge your report within 48 hours and aim to provide a fix within 7 days for critical issues.

## Supported Versions

| Version | Supported |
|---------|-----------|
| Latest release | Yes |
| Older releases | No |

We recommend always using the latest release.

## Security Features

- **Credential storage**: Tokens are stored in `~/.fastio/credentials.json` with `0600` file permissions (Unix)
- **Secret handling**: Sensitive values use `secrecy::SecretString` with memory zeroization on drop
- **TLS**: All API communication uses HTTPS with platform-native TLS
- **PKCE**: OAuth authentication uses RFC 7636 S256 challenge method
- **No shell execution**: The CLI does not execute shell commands with user-supplied input
- **URL encoding**: All API path parameters are encoded to prevent injection
- **Filename sanitization**: Downloaded filenames are sanitized to prevent path traversal
