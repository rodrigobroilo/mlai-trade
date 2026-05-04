# Test Harness Assets

This directory stores repo-owned test infrastructure that is not part of the
runtime binary.

- `linux-ubuntu/`: Docker image definition used by
  `scripts/linux-ubuntu-test.sh` on non-Linux hosts.
- `freebsd-lima/`: notes for the Lima-backed FreeBSD validation path used by
  `scripts/freebsd-lima-test.sh`.

Executable entrypoints stay in `scripts/`.
