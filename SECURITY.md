# Security Policy

## Supported versions

`ash` has not published an executable release. There is currently no supported
runtime version. Security fixes will target the latest maintained release once
releases begin.

| Version | Supported |
| --- | --- |
| No published release | Not applicable |

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability.

Use GitHub's private vulnerability reporting for `A3S-Lab/ash` when the
repository offers the **Report a vulnerability** action. If that action is not
available, contact an A3S Lab maintainer through GitHub without including
sensitive details and request a private channel.

Include, when available:

- the affected commit or release;
- operating system and architecture;
- a minimal reproduction;
- the expected security boundary;
- the observed impact;
- whether disclosure is time-sensitive; and
- any known workaround.

We aim to acknowledge a complete report within five business days and to
coordinate disclosure after a fix or mitigation is available.

## Security-sensitive areas

Reports are especially useful for:

- ASON framing, parsing, canonicalization, and resource limits;
- workspace escape through paths, symbolic links, or Windows reparse points;
- capability or approval-permit bypass;
- process-tree ownership, cancellation, and cleanup;
- retained-result spool permissions, quota enforcement, lease lifetime, and cleanup;
- unsafe file mutation or rollback behavior;
- secret exposure through output reduction or retained results; and
- installer, updater, signature, provenance, or rollback failures.

Executing a command explicitly authorized by the caller is not by itself a
vulnerability. A boundary bypass, unexpected privilege use, or behavior that
contradicts the documented capability model may be one.
