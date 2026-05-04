# Third-Party License Notes

Last checked: 2026-05-02

This crate is licensed as MIT. Its resolved Rust dependency tree was checked with:

```sh
cargo metadata --format-version 1 --all-features \
  | jq -r '.packages[] | select(.source != null or .name=="alpaca") | [.name,.version,(.license // .license_file // "UNKNOWN"),(.repository // ""),(.source // "local")] | @tsv' \
  | sort -u
```

The full one-crate-per-line inventory is stored in `THIRD_PARTY_CRATES.tsv`. It includes crate name, version, license expression, repository, and source. The current all-features inventory has 333 resolved entries plus the header row.

The resolved dependencies are compatible with an MIT-licensed application based on the metadata checked on this date. No package in `THIRD_PARTY_CRATES.tsv` has an `UNKNOWN` license expression. Most dependencies are MIT, Apache-2.0, BSD, ISC, Unicode-3.0, Zlib, Unlicense, CC0-1.0, or similarly permissive. `r-efi` appears in the graph as `MIT OR Apache-2.0 OR LGPL-2.1-or-later`; this project relies on the MIT or Apache-2.0 option.

Optional XGBoost baseline support adds `xgboost_lib-sys` under MIT and `openmp-sys` under CC0-1.0. The XGBoost feature links a native XGBoost library through the C API and may require platform OpenMP runtime packages such as Homebrew `libomp` on macOS or the distribution OpenMP runtime on Linux.

Optional accelerated LSTM support adds `mlx-rs` and `mlx-macros` under MIT OR Apache-2.0 for Apple Silicon builds and `tch`/`torch-sys` under MIT/Apache-2.0 for Linux CUDA builds. These optional backends may require platform SDK/runtime components that are not vendored in this repository.

`lightgbm3-sys` includes and statically builds LightGBM source from its crate package. The vendored LightGBM license file in the resolved crate is MIT.

The local `alpaca` crate should not copy third-party source code into this repository unless that source license is reviewed first. Generated model files, SQLite data, and downloaded market data are runtime artifacts, not source license grants.

This note is a developer review record, not legal advice.
