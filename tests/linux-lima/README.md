# Linux Lima Test Harness

The Linux validation entrypoint is `scripts/linux-lima-test.sh`.

On Linux hosts the script runs directly on the host. On non-Linux hosts it uses
a cached Lima Ubuntu 24.04 VM, defaulting to x86_64 so the mandatory Linux
`tch`/libtorch path matches upstream libtorch binaries. The VM is stored
outside the repository at `~/.lima/mlai-trade-linux-amd64-test`. The repo copy
inside the guest is staged at `/tmp/mlai-trade-src`, and the guest Cargo target
cache is `/tmp/mlai-trade-target`.

No VM disk image, credentials, generated logs, or test database files belong in
this directory.
