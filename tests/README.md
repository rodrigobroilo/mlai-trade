# Test Harness Assets

This directory stores repo-owned test infrastructure that is not part of the
runtime binary.

- `linux-lima/`: notes for the Lima-backed Ubuntu validation path used by
  `scripts/linux-lima-test.sh` on non-Linux hosts.
- `freebsd-lima/`: notes for the Lima-backed FreeBSD validation path used by
  `scripts/freebsd-lima-test.sh`.

Executable entrypoints stay in `scripts/`.
