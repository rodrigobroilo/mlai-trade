# Third-Party License Notes

Last checked: 2026-05-07

This crate is licensed as MIT. Its resolved Rust dependency tree was checked with:

```sh
cargo metadata --format-version 1 --all-features --locked \
  | jq -r '.packages[] |
      [
        .name,
        .version,
        (.license // .license_file // "UNKNOWN"),
        (.repository // ""),
        (.source // "local")
      ] | @tsv' \
  | sort -u
```

The full one-crate-per-line inventory is stored in `THIRD_PARTY_CRATES.tsv`.
It includes crate name, version, license expression, repository, and source.
The current all-target inventory has 344 resolved entries plus the header row.

The React dashboard under `api/html/` is resolved by `package-lock.json`. The
top-level frontend dependencies are React, React DOM, Vite, and
`@vitejs/plugin-react`; all are permissively licensed in their npm metadata.
The production build is committed under `api/html/` as `index.html`, `assets/`,
and `robots.txt` so runtime deployments do not need npm or a network connection
to serve the dashboard.

The resolved dependencies are compatible with an MIT-licensed application based on the metadata checked on this date. No package in `THIRD_PARTY_CRATES.tsv` has an `UNKNOWN` license expression. Most dependencies are MIT, Apache-2.0, BSD, ISC, Unicode-3.0, Zlib, Unlicense, CC0-1.0, or similarly permissive. `r-efi` appears in the graph as `MIT OR Apache-2.0 OR LGPL-2.1-or-later`; this project relies on the MIT or Apache-2.0 option.

XGBoost baseline support is mandatory on macOS and Linux builds and adds
`xgboost_lib-sys` under MIT and `openmp-sys` under CC0-1.0. It links a native
XGBoost library through the C API and may require platform OpenMP runtime
packages such as Homebrew `libomp` on macOS or the distribution OpenMP runtime
on Linux.

Accelerated LSTM support adds `mlx-rs` and `mlx-macros` under MIT OR
Apache-2.0 for Apple Silicon builds and `tch`/`torch-sys` under MIT/Apache-2.0
for Linux CUDA builds. The Linux `tch` build uses `download-libtorch`, which
adds `ureq` and the older `webpki-roots` 0.26 dependency path for libtorch
retrieval. These backends may require platform SDK/runtime components that are
not vendored in this repository.

`lightgbm3-sys` includes and statically builds LightGBM source from its crate package. The vendored LightGBM license file in the resolved crate is MIT.

The local `mlai-trade` crate should not copy third-party source code into this
repository unless that source license is reviewed first. Generated model files,
SQLite data, and downloaded market data are runtime artifacts, not source
license grants.

This note is a developer review record, not legal advice.
