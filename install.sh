#!/bin/bash
set -euo pipefail

usage() {
    printf '%s\n' \
        'Usage: bash install.sh [--with-debugger | --native-only] [--prefix PATH] [--dry-run]' \
        '  No mode: download the published pre-release (macOS ARM64, /usr/local).' \
        '  --with-debugger: install target/release/chasm from this checkout, plus its local worker.' \
        '    Requires uv, Rust 1.91+ and a native build toolchain. uv uses Python 3.12 and the repo locks.' \
        '  --native-only: install just target/release/chasm from this checkout.' \
        '  Checkout installs default to ~/.local; choose a user-writable prefix for the worker.' \
        '  --dry-run: print inputs and destinations without installing or downloading anything.'
}
die() { printf 'Error: %s\n' "$*" >&2; exit 1; }

mode=release
prefix=
dry_run=false
while (($#)); do
    case "$1" in
        --with-debugger|--native-only)
            [[ $mode == release ]] || die 'Choose one installation mode.'
            mode=${1#--} ;;
        --prefix) shift; [[ $# -gt 0 && -n $1 ]] || die '--prefix needs a path.'; prefix=$1 ;;
        --dry-run) dry_run=true ;;
        -h|--help) usage; exit 0 ;;
        *) die "Unknown option: $1 (see --help)." ;;
    esac
    shift
done

if [[ $mode == release ]]; then
    prefix=${prefix:-/usr/local}
    source_binary=https://github.com/ChasmHQ/chasm/releases/download/pre-release/chasm
    [[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || die 'The published binary is macOS ARM64 only. On Linux/macOS, build this checkout with cargo build --release --locked, then use --with-debugger or --native-only.'
else
    [[ -n ${BASH_SOURCE[0]:-} ]] || die 'Run the checkout installer from its file, not standard input.'
    root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
    prefix=${prefix:-"$HOME/.local"}
    source_binary=$root/target/release/chasm
    source_worker=$root/debugger
fi
[[ $prefix == /* ]] || die '--prefix must be an absolute path.'
bin_dir=${prefix%/}/bin
dest=$bin_dir/chasm
worker=${prefix%/}/share/chasm/debugger
assets=(examples/mainnet/synthetic-replay.json examples/web-debugger/{source-replay.json,build-info.json,WorkshopVault.sol})
printf 'Binary: %s -> %s\n' "$source_binary" "$dest"
if [[ $mode == with-debugger ]]; then
    printf 'Worker: %s -> %s (private venv; dependencies may be downloaded)\n' "$source_worker" "$worker"
else
    printf 'No debugger will be installed or updated.\n'
fi
$dry_run && exit 0

if [[ $mode != release && ! -x $source_binary ]]; then
    die 'No matching release executable. Run cargo build --release --locked in this checkout first.'
fi
if [[ $mode == with-debugger ]]; then
    [[ ! -e $worker && ! -L $worker ]] || die "Refusing to replace $worker. Uninstall the managed worker or choose a new --prefix."
    for file in uv.lock Cargo.lock pyproject.toml rust/python/Cargo.toml "${assets[@]}"; do
        [[ -f $source_worker/$file ]] || die "Missing checkout file: debugger/$file"
    done
    command -v uv >/dev/null && command -v cargo >/dev/null || die 'Install uv and Rust 1.91+ before setting up the worker.'
fi
if ! command -v forge >/dev/null; then
    printf 'Chasm requires Foundry (anvil, cast, forge): https://getfoundry.sh\n'
    read -r -p 'Continue installation anyway? (y/N) ' answer || exit 1
    [[ $answer == [yY] ]] || exit 1
fi

run_install() {
    if [[ $mode == release && ! -w $bin_dir && ( -d $bin_dir || ! -w $prefix ) ]]; then sudo "$@"; else "$@"; fi
}
run_install mkdir -p "$bin_dir"
[[ ! -L $dest && ! -d $dest ]] || die "Refusing to overwrite non-regular destination: $dest"
[[ $mode == release || -w $bin_dir ]] || die "Choose a user-writable --prefix (cannot write $bin_dir)."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
if [[ $mode == release ]]; then
    curl --fail --location --progress-bar "$source_binary" -o "$tmp/chasm"
else
    cp "$source_binary" "$tmp/chasm"
fi
chmod 755 "$tmp/chasm"

if [[ $mode == with-debugger ]]; then
    mkdir -p "$(dirname "$worker")"
    mkdir "$worker"
    # A venv's entry points contain absolute paths. Build it at its final location.
    printf 'chasm-desktop-worker-v1\n' > "$worker/.chasm-installed"
    printf 'If setup fails, run bash uninstall.sh --worker-only --prefix "%s" before retrying.\n' "$prefix"
    for asset in "${assets[@]}"; do
        mkdir -p "$worker/$(dirname "$asset")"
        cp "$source_worker/$asset" "$worker/$asset"
    done
    python=$worker/.venv/bin/python
    UV_PROJECT_ENVIRONMENT="$worker/.venv" uv sync --project "$source_worker" --python 3.12 --locked --no-dev --no-install-project
    # Match the build backend in debugger/uv.lock; keep its CLI outside the runtime venv.
    (cd "$source_worker" && uv tool run --from maturin==1.15.0 maturin build --release --locked --interpreter "$python" --out "$tmp")
    wheels=("$tmp"/*.whl)
    [[ ${#wheels[@]} == 1 && -f ${wheels[0]} ]] || die 'Expected one worker wheel from maturin.'
    uv pip install --no-index --no-deps --python "$python" "${wheels[0]}"
    "$python" -I -c 'from chasm_debugger import _revm, web_service'
fi
run_install mv -f "$tmp/chasm" "$dest"
printf 'Installed %s. Run: "%s" .\n' "$dest" "$dest"
case ":$PATH:" in *":$bin_dir:"*) ;; *) printf 'To run by name, add %s to PATH.\n' "$bin_dir" ;; esac
