# Security policy

## Supported versions

Remnant is pre-1.0 software. Security fixes are made on the latest `main`
revision and included in the next release.

## Reporting a vulnerability

Do not open a public issue for a vulnerability. Use GitHub's private
**Security advisories → Report a vulnerability** flow for this repository. If
that flow is unavailable, contact the maintainer through the private contact
method on their GitHub profile. Include the affected revision, impact,
reproduction steps, and any suggested mitigation. Please do not include real
credentials or production data.

## Threat model and trust boundaries

Remnant is a development and debugging tool, not a production data-plane
service. Its safety model assumes:

- the operator trusts the project configuration and oracle command;
- PostgreSQL and Redis targets are isolated, disposable test instances;
- environment fingerprints are verified before destructive restore actions;
- credentials are supplied through environment variables and kept out of
  configuration, logs, reports, and source control;
- the MCP stdio peer is local and trusted.

The project file and oracle command are executable input. A malicious project
can run arbitrary commands with the operator's permissions and can point at
valuable infrastructure. Reports and snapshots can contain sensitive data.
MCP deliberately exposes a bounded tool set, but it does not turn an untrusted
project or database into trusted input.

## Safe operation

1. Run Remnant in a disposable VM, container, or isolated development account.
2. Use least-privilege credentials that cannot reach production.
3. Review `remnant.yaml` and the oracle command before running it.
4. Verify the environment fingerprint and backup/restore behavior on throwaway
   data first.
5. Protect snapshots, session databases, and reports as sensitive artifacts;
   delete them according to your data-retention policy.
6. Never expose the MCP stdio transport through an unauthenticated network
   bridge.

## Out of scope guarantees

Remnant does not provide sandboxing for oracle commands, authorization for
database servers, encryption at rest, secret management, or a guarantee that a
third-party adapter is non-destructive. Production use is unsupported.
