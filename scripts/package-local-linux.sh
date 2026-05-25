#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

detect_nvidia() {
  command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi -L >/dev/null 2>&1
}

nvcc_version_key() {
  local nvcc_bin="$1"
  "$nvcc_bin" --version 2>/dev/null \
    | sed -n 's/.*release \([0-9][0-9]*\)\.\([0-9][0-9]*\).*/\1 \2/p' \
    | awk '{ printf "%03d%03d\n", $1, $2; exit }'
}

cuda_root_for_nvcc() {
  local nvcc_bin="$1"
  local real_nvcc
  real_nvcc="$(readlink -f "$nvcc_bin" 2>/dev/null || printf '%s' "$nvcc_bin")"
  cd "$(dirname "$real_nvcc")/.." && pwd
}

cuda_has_headers_and_libs() {
  local root="$1"
  [[ -f "$root/include/cuda.h" || -f "$root/targets/x86_64-linux/include/cuda.h" ]] \
    && [[ -d "$root/lib64" || -d "$root/targets/x86_64-linux/lib" ]]
}

find_best_cuda_root() {
  local candidates=()
  local root nvcc_bin version

  for root in "${CUDA_HOME:-}" "${CUDA_PATH:-}" /usr/local/cuda /usr/local/cuda-*; do
    [[ -n "$root" && -x "$root/bin/nvcc" ]] && candidates+=("$root/bin/nvcc")
  done
  if command -v nvcc >/dev/null 2>&1; then
    candidates+=("$(command -v nvcc)")
  fi

  for nvcc_bin in "${candidates[@]}"; do
    root="$(cuda_root_for_nvcc "$nvcc_bin")"
    version="$(nvcc_version_key "$root/bin/nvcc")"
    [[ -n "$version" ]] || continue
    cuda_has_headers_and_libs "$root" || continue
    printf '%s\t%s\n' "$version" "$root"
  done | sort -r -u | awk 'NR == 1 { print $2 }'
}

detect_cuda_toolchain() {
  command -v cmake >/dev/null 2>&1 \
    && command -v ninja >/dev/null 2>&1 \
    && command -v git >/dev/null 2>&1 \
    && [[ -n "$(find_best_cuda_root)" ]]
}

detect_cuda_arches() {
  nvidia-smi --query-gpu=compute_cap --format=csv,noheader,nounits 2>/dev/null \
    | awk -F. 'NF >= 2 { print $1 $2 }' \
    | sort -n -u \
    | paste -sd ';' -
}

test_cuda_host_compiler() {
  local nvcc_bin="$1"
  local cxx_bin="$2"
  local tmpdir
  tmpdir="$(mktemp -d)"
  printf '__global__ void k() {}\nint main() { return 0; }\n' > "$tmpdir/test.cu"
  if [[ -n "$cxx_bin" ]]; then
    "$nvcc_bin" -ccbin "$cxx_bin" -c "$tmpdir/test.cu" -o "$tmpdir/test.o" >/dev/null 2>&1
  else
    "$nvcc_bin" -c "$tmpdir/test.cu" -o "$tmpdir/test.o" >/dev/null 2>&1
  fi
  local status=$?
  rm -rf "$tmpdir"
  return "$status"
}

find_cuda_host_compiler() {
  local nvcc_bin="$1"
  local candidate
  local candidates=("")

  for candidate in "${CUDAHOSTCXX:-}" "$(command -v g++ 2>/dev/null || true)" /usr/bin/g++-*; do
    [[ -n "$candidate" && -x "$candidate" ]] && candidates+=("$candidate")
  done

  for candidate in "${candidates[@]}"; do
    if test_cuda_host_compiler "$nvcc_bin" "$candidate"; then
      if [[ -n "$candidate" ]]; then
        printf '%s\n' "$candidate"
      else
        printf '__default__\n'
      fi
      return 0
    fi
  done

  return 1
}

make_nvcc_arch_wrapper() {
  local wrapper_dir="$repo_root/target/native/cuda"
  local wrapper="$wrapper_dir/nvcc-arch-wrapper"
  mkdir -p "$wrapper_dir"
  cat > "$wrapper" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

real_nvcc="${MLAI_TRADE_REAL_NVCC:?MLAI_TRADE_REAL_NVCC is required}"
allowed_arches="${MLAI_TRADE_NVCC_ALLOWED_ARCHES:?MLAI_TRADE_NVCC_ALLOWED_ARCHES is required}"

arch_arg_allowed() {
  local arg="$1"
  local arch
  local has_arch=0

  if [[ "$arg" == *compute_* || "$arg" == *sm_* ]]; then
    has_arch=1
  fi

  IFS=';' read -r -a arches <<< "$allowed_arches"
  for arch in "${arches[@]}"; do
    if [[ "$arg" == *"compute_${arch}"* || "$arg" == *"sm_${arch}"* ]]; then
      return 0
    fi
  done

  [[ "$has_arch" -eq 0 ]]
}

args=()
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --generate-code|-gencode)
      opt="$1"
      shift
      if [[ "$#" -eq 0 ]]; then
        args+=("$opt")
        break
      fi
      if arch_arg_allowed "$1"; then
        args+=("$opt" "$1")
      fi
      shift
      ;;
    --generate-code=*|-gencode=*)
      if arch_arg_allowed "$1"; then
        args+=("$1")
      fi
      shift
      ;;
    *)
      args+=("$1")
      shift
      ;;
  esac
done

exec "$real_nvcc" "${args[@]}"
EOF
  chmod +x "$wrapper"
  printf '%s\n' "$wrapper"
}

reset_stale_lightgbm_cuda_cache() {
  local nvcc_wrapper="$1"
  local cache build_dir

  for cache in "$release_dir"/build/lightgbm3-sys-*/out/build/CMakeCache.txt; do
    [[ -f "$cache" ]] || continue
    if grep -q '^CMAKE_CUDA_COMPILER:FILEPATH=' "$cache" \
      && ! grep -Fq "CMAKE_CUDA_COMPILER:FILEPATH=$nvcc_wrapper" "$cache"; then
      build_dir="$(dirname "$cache")"
      echo "Removing stale LightGBM CUDA CMake cache: $build_dir" >&2
      rm -rf "$build_dir"
    fi
  done
}

reset_stale_libtorch_cache() {
  local desired_device="$1"
  local libtorch_root lib_dir marker current_marker

  [[ -z "${LIBTORCH:-}" ]] || return 0

  for libtorch_root in "$release_dir"/build/torch-sys-*/out/libtorch; do
    [[ -d "$libtorch_root" ]] || continue
    lib_dir="$libtorch_root/libtorch/lib"
    marker="$libtorch_root/.mlai-trade-device"
    current_marker="$(cat "$marker" 2>/dev/null || true)"

    if [[ "$desired_device" == cuda=* ]]; then
      if [[ "$current_marker" != "$desired_device" || ! -f "$lib_dir/libtorch_cuda.so" ]]; then
        echo "Removing stale libtorch cache for $desired_device: $libtorch_root" >&2
        rm -rf "$libtorch_root"
      fi
    elif [[ -f "$lib_dir/libtorch_cuda.so" || "$current_marker" == cuda=* ]]; then
      echo "Removing stale CUDA libtorch cache for CPU package: $libtorch_root" >&2
      rm -rf "$libtorch_root"
    fi
  done
}

mark_libtorch_cache() {
  local desired_device="$1"
  local libtorch_root

  [[ -z "${LIBTORCH:-}" ]] || return 0

  for libtorch_root in "$release_dir"/build/torch-sys-*/out/libtorch; do
    [[ -d "$libtorch_root/libtorch/lib" ]] || continue
    printf '%s\n' "$desired_device" > "$libtorch_root/.mlai-trade-device"
  done
}

parallel_jobs() {
  nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || printf '2\n'
}

runtime_search_dirs=()
runtime_allowed_roots=()

add_runtime_search_dir() {
  local dir="$1"
  local existing

  [[ -n "$dir" && -d "$dir" ]] || return 0
  dir="$(cd "$dir" && pwd)"
  for existing in "${runtime_search_dirs[@]:-}"; do
    [[ "$existing" == "$dir" ]] && return 0
  done
  runtime_search_dirs+=("$dir")
}

add_runtime_allowed_root() {
  local dir="$1"
  local existing

  [[ -n "$dir" && -d "$dir" ]] || return 0
  dir="$(cd "$dir" && pwd)"
  for existing in "${runtime_allowed_roots[@]:-}"; do
    [[ "$existing" == "$dir" ]] && return 0
  done
  runtime_allowed_roots+=("$dir")
}

runtime_ld_library_path() {
  local path
  path="$(IFS=:; printf '%s' "${runtime_search_dirs[*]:-}")"
  if [[ -n "${LD_LIBRARY_PATH:-}" ]]; then
    if [[ -n "$path" ]]; then
      printf '%s:%s\n' "$path" "$LD_LIBRARY_PATH"
    else
      printf '%s\n' "$LD_LIBRARY_PATH"
    fi
  else
    printf '%s\n' "$path"
  fi
}

runtime_path_allowed() {
  local path="$1"
  local root

  [[ "$path" = /* && -f "$path" ]] || return 1
  for root in "${runtime_allowed_roots[@]:-}"; do
    case "$path" in
      "$root"/*) return 0 ;;
    esac
  done
  return 1
}

runtime_dependency_paths() {
  local elf="$1"
  LD_LIBRARY_PATH="$(runtime_ld_library_path)" ldd "$elf" \
    | awk '
        /=>/ && $(NF - 1) ~ /^\// { print $(NF - 1); next }
        /^[[:space:]]*\// { print $1 }
      '
}

copy_runtime_dependency_closure() {
  local queue=("$@")
  local seen="|"
  local file dep dep_real dest base

  while [[ "${#queue[@]}" -gt 0 ]]; do
    file="${queue[0]}"
    queue=("${queue[@]:1}")
    [[ -e "$file" ]] || continue
    file="$(readlink -f "$file" 2>/dev/null || printf '%s' "$file")"
    case "$seen" in
      *"|$file|"*) continue ;;
    esac
    seen+="$file|"

    while IFS= read -r dep; do
      [[ -n "$dep" ]] || continue
      dep_real="$(readlink -f "$dep" 2>/dev/null || printf '%s' "$dep")"
      runtime_path_allowed "$dep_real" || continue
      base="$(basename "$dep")"
      dest="$lib_dir/$base"
      if [[ ! -f "$dest" ]]; then
        cp -L "$dep" "$dest"
      fi
      queue+=("$dest")
    done < <(runtime_dependency_paths "$file")
  done
}

ensure_cuda_xgboost() {
  local cuda_root="$1"
  local cuda_arches="$2"
  local cuda_host_cxx="$3"
  local version="${MLAI_TRADE_XGBOOST_VERSION:-v3.2.0}"
  local version_name="${version#v}"
  local native_root="$repo_root/target/native/xgboost-$version_name-cuda"
  local source_dir="$native_root/source"
  local build_dir="$native_root/build"
  local install_dir="$native_root/install"
  local stamp_file="$install_dir/.mlai-trade-build"
  local build_key
  local cmake_args
  local candidate

  build_key="version=$version cuda=$("$cuda_root/bin/nvcc" --version | sed -n 's/.*release \([^,]*\),.*/\1/p') arch=$cuda_arches host=${cuda_host_cxx/__default__/default}"

  for candidate in "$install_dir/lib" "$install_dir/lib64"; do
    if [[ -f "$candidate/libxgboost.so" && -f "$stamp_file" && "$(cat "$stamp_file")" == "$build_key" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done

  if [[ ! -d "$source_dir/.git" ]]; then
    if [[ -e "$source_dir" ]]; then
      echo "error: $source_dir exists but is not a git checkout; move it aside or set MLAI_TRADE_XGBOOST_VERSION to a different tag." >&2
      return 1
    fi
    mkdir -p "$native_root"
    echo "Fetching upstream XGBoost $version for CUDA packaging" >&2
    git clone --recursive --branch "$version" --depth 1 https://github.com/dmlc/xgboost.git "$source_dir" >&2
  fi

  git -C "$source_dir" submodule update --init --recursive --depth 1 >&2

  cmake_args=(
    -S "$source_dir"
    -B "$build_dir"
    -G Ninja
    -DCMAKE_BUILD_TYPE=Release
    -DCMAKE_INSTALL_PREFIX="$install_dir"
    -DCMAKE_CUDA_COMPILER="$cuda_root/bin/nvcc"
    -DCUDAToolkit_ROOT="$cuda_root"
    -DCMAKE_CUDA_ARCHITECTURES="$cuda_arches"
    -DUSE_CUDA=ON
    -DUSE_NCCL=OFF
    -DBUILD_STATIC_LIB=OFF
  )
  if [[ "$cuda_host_cxx" != "__default__" ]]; then
    cmake_args+=("-DCMAKE_CUDA_HOST_COMPILER=$cuda_host_cxx")
  fi

  echo "Building upstream XGBoost $version with CUDA" >&2
  cmake "${cmake_args[@]}" >&2
  cmake --build "$build_dir" --target install --parallel "$(parallel_jobs)" >&2

  for candidate in "$install_dir/lib" "$install_dir/lib64" "$build_dir/lib" "$build_dir"; do
    if [[ -f "$candidate/libxgboost.so" ]]; then
      mkdir -p "$install_dir"
      printf '%s\n' "$build_key" > "$stamp_file"
      printf '%s\n' "$candidate"
      return 0
    fi
  done

  echo "error: upstream XGBoost CUDA build finished but libxgboost.so was not found." >&2
  return 1
}

cuda_mode="${MLAI_TRADE_CUDA:-auto}"
cuda_enabled=0
case "${cuda_mode,,}" in
  1|true|yes|on|cuda|nvidia)
    if ! detect_nvidia; then
      echo "error: MLAI_TRADE_CUDA=1 was requested, but nvidia-smi cannot see an NVIDIA GPU." >&2
      exit 1
    fi
    if ! detect_cuda_toolchain; then
      echo "error: MLAI_TRADE_CUDA=1 was requested, but nvcc/cmake/ninja or /usr/local/cuda is missing." >&2
      exit 1
    fi
    cuda_enabled=1
    ;;
  0|false|no|off|cpu)
    cuda_enabled=0
    ;;
  auto)
    if detect_nvidia; then
      if detect_cuda_toolchain; then
        cuda_enabled=1
      else
        echo "warning: NVIDIA GPU detected, but CUDA build toolchain is incomplete; packaging CPU build." >&2
      fi
    fi
    ;;
  *)
    echo "error: MLAI_TRADE_CUDA must be auto, 1, or 0; got '$MLAI_TRADE_CUDA'." >&2
    exit 1
    ;;
esac

release_dir="$repo_root/target/release"
bin_dir="$repo_root/bin"
lib_dir="$bin_dir/lib"
tools_dir="$bin_dir/tools"
cuda_mode_normalized="${cuda_mode,,}"
cargo_args=(cargo build --release)
torch_device_key="cpu"
add_runtime_allowed_root "$repo_root"
if [[ -n "${LIBTORCH:-}" ]]; then
  add_runtime_allowed_root "$LIBTORCH"
fi
if [[ "$cuda_enabled" -eq 1 ]]; then
  cuda_root="$(find_best_cuda_root)"
  add_runtime_allowed_root "$cuda_root"
  cuda_arches="${MLAI_TRADE_CUDA_ARCHES:-$(detect_cuda_arches)}"
  cuda_host_cxx="$(find_cuda_host_compiler "$cuda_root/bin/nvcc" || true)"
  if [[ -z "$cuda_arches" ]]; then
    echo "error: CUDA build requested, but GPU compute capability could not be detected." >&2
    exit 1
  fi
  if [[ -z "$cuda_host_cxx" ]]; then
    echo "error: CUDA build requested, but no working CUDA host C++ compiler was found." >&2
    exit 1
  fi

  export PATH="$cuda_root/bin:$PATH"
  export CUDA_HOME="$cuda_root"
  export CUDA_PATH="$cuda_root"
  export CUDAToolkit_ROOT="$cuda_root"
  export MLAI_TRADE_REAL_NVCC="$cuda_root/bin/nvcc"
  export MLAI_TRADE_NVCC_ALLOWED_ARCHES="$cuda_arches"
  nvcc_wrapper="$(make_nvcc_arch_wrapper)"
  export CUDACXX="$nvcc_wrapper"
  export CMAKE_CUDA_COMPILER="$nvcc_wrapper"
  if [[ "$cuda_host_cxx" != "__default__" ]]; then
    export CUDAHOSTCXX="$cuda_host_cxx"
  fi
  export CUDAARCHS="$cuda_arches"
  export CMAKE_CUDA_ARCHITECTURES="$cuda_arches"
  export TORCH_CUDA_VERSION="${MLAI_TRADE_TORCH_CUDA_VERSION:-cu128}"
  torch_device_key="cuda=$TORCH_CUDA_VERSION"

  echo "Packaging NVIDIA CUDA build for supported GPU backends"
  echo "  CUDA toolkit: $cuda_root ($("$cuda_root/bin/nvcc" --version | sed -n 's/.*release \([^,]*\),.*/\1/p'))"
  echo "  CUDA arch:    $cuda_arches"
  echo "  Host C++:     ${cuda_host_cxx/__default__/default}"
  echo "  NVCC wrapper: $nvcc_wrapper"
  echo "  libtorch:     $TORCH_CUDA_VERSION"
  reset_stale_lightgbm_cuda_cache "$nvcc_wrapper"
  reset_stale_libtorch_cache "$torch_device_key"
  if xgboost_lib_dir="$(ensure_cuda_xgboost "$cuda_root" "$cuda_arches" "$cuda_host_cxx")"; then
    echo "  XGBoost:      $xgboost_lib_dir/libxgboost.so"
    mkdir -p "$release_dir/deps"
    cp -L "$xgboost_lib_dir/libxgboost.so" "$release_dir/deps/libxgboost.so"
    export XGBOOST_LIB_DIR="$xgboost_lib_dir"
    cargo_args+=(--no-default-features --features nvidia-cuda)
  elif [[ "$cuda_mode_normalized" == "auto" ]]; then
    echo "warning: CUDA XGBoost build failed; packaging CPU build." >&2
    cuda_enabled=0
  else
    exit 1
  fi
else
  echo "Packaging CPU build"
  reset_stale_libtorch_cache "$torch_device_key"
fi

if ! "${cargo_args[@]}"; then
  if [[ "$cuda_enabled" -eq 1 && "$cuda_mode_normalized" == "auto" ]]; then
    echo "warning: CUDA package build failed; falling back to CPU package." >&2
    cuda_enabled=0
    cargo build --release
  else
    exit 1
  fi
fi
mark_libtorch_cache "$torch_device_key"

mkdir -p "$lib_dir"
mkdir -p "$tools_dir"
find "$lib_dir" -maxdepth 1 -type f -name '*.so*' -delete
find "$tools_dir" -maxdepth 1 -type f -delete
cp "$release_dir/mlai-trade" "$bin_dir/mlai-trade"
if [[ -L "$release_dir/lib" || ! -e "$release_dir/lib" ]]; then
  ln -sfn "../../bin/lib" "$release_dir/lib"
fi

add_runtime_search_dir "$lib_dir"
add_runtime_search_dir "$release_dir/deps"
if [[ -n "${xgboost_lib_dir:-}" ]]; then
  add_runtime_search_dir "$xgboost_lib_dir"
fi
for xgboost_dir in \
  "$release_dir"/build/xgboost_lib-sys-*/out/lib \
  "$release_dir"/build/xgboost_lib-sys-*/out/lib64 \
  "$release_dir"/build/xgboost_lib-sys-*/out/build; do
  add_runtime_search_dir "$xgboost_dir"
done
for lightgbm_bin in "$release_dir"/build/lightgbm3-sys-*/out/bin/lightgbm; do
  [[ -x "$lightgbm_bin" ]] || continue
  cp -L "$lightgbm_bin" "$tools_dir/lightgbm"
  break
done
for torch_lib_dir in "$release_dir"/build/torch-sys-*/out/libtorch/libtorch/lib; do
  add_runtime_search_dir "$torch_lib_dir"
done
if [[ "$cuda_enabled" -eq 1 && -n "${cuda_root:-}" ]]; then
  for cuda_lib_dir in "$cuda_root/lib64" "$cuda_root/targets/x86_64-linux/lib" "$cuda_root/lib"; do
    add_runtime_search_dir "$cuda_lib_dir"
  done
fi

copy_runtime_dependency_closure "$bin_dir/mlai-trade"
if [[ -x "$tools_dir/lightgbm" ]]; then
  copy_runtime_dependency_closure "$tools_dir/lightgbm"
fi

missing="$(ldd "$bin_dir/mlai-trade" | awk '/not found/ { print $1 }')"
if [[ -n "$missing" ]]; then
  echo "error: unresolved shared libraries after packaging:" >&2
  echo "$missing" >&2
  exit 1
fi

echo "Packaged $bin_dir/mlai-trade with runtime libraries in $lib_dir"
