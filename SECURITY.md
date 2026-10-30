# Security policy

This is an independently maintained preview. Security fixes target the latest
desktop preview; older previews do not have a support commitment.

Please report vulnerabilities through this repository's **Security → Report a
vulnerability** if private reporting is available. Otherwise open a minimal
issue requesting a private reporting channel, without exploit details or
private data. Do not include private keys, personal received files, device
pairing lists, credentials, or unredacted logs in public reports.

The application is intended for reachable, trusted local networks. HTTPS uses
per-device certificates; only pair devices you trust, because paired devices
are automatically accepted. Browser-link mode uses plain HTTP and must not be
used for sensitive content. The backend listens on network interfaces while
running; stop it with `localsend stop` when it is not needed. Installation does
not change firewall rules or add autostart.

The local control socket is private to the current user (0600 plus UID checks).
Receive data is verified before CLI publication. This preview has not undergone
an independent security audit and does not promise all peer/OS combinations
have been tested. See the README for filesystem and directory-transfer limits.
