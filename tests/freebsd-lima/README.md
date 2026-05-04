# FreeBSD Lima Test Harness

The FreeBSD validation entrypoint remains `scripts/freebsd-lima-test.sh`.

The script uses Lima's FreeBSD template by default and stores the VM outside the
repository at `~/.lima/mlai-trade-freebsd16-test`. The repo copy inside the
guest is staged at `/tmp/mlai-trade-src`.

No VM disk image, credentials, generated logs, or test database files belong in
this directory.
