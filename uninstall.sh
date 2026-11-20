#!/bin/bash
set -euo pipefail

die() { printf 'Error: %s\n' "$*" >&2; exit 1; }
prefix=/usr/local
worker_only=false
dry_run=false
while (($#)); do
    case "$1" in
        --prefix) shift; [[ $# -gt 0 && -n $1 ]] || die '--prefix needs a path.'; prefix=$1 ;;
        --worker-only) worker_only=true ;;
        --dry-run) dry_run=true ;;
        -h|--help)
            printf '%s\n' 'Usage: bash uninstall.sh [--prefix PATH] [--worker-only] [--dry-run]' \
                'Default: /usr/local (published binary). Use --prefix "$HOME/.local" for a default checkout install.' \
                'Only installer-managed worker directories are removed. User data is kept.'
            exit 0 ;;
        *) die "Unknown option: $1 (see --help)." ;;
    esac
    shift
done
[[ $prefix == /* ]] || die '--prefix must be an absolute path.'
bin=${prefix%/}/bin/chasm
worker=${prefix%/}/share/chasm/debugger
$worker_only || printf 'Binary: %s\n' "$bin"
printf 'Managed worker: %s\n' "$worker"
$dry_run && exit 0

if [[ -e $worker || -L $worker ]]; then
    marker=$worker/.chasm-installed
    if [[ ! -L $worker && -d $worker && ! -L $marker && -f $marker && $(<"$marker") == chasm-desktop-worker-v1 ]]; then
        rm -rf "$worker"
        printf 'Removed the worker.\n'
    else
        printf 'Left unrecognized directory or link untouched: %s\n' "$worker" >&2
        if $worker_only; then exit 1; fi
    fi
else
    printf 'No worker installed at this prefix.\n'
fi
if ! $worker_only; then
    [[ -f $bin && ! -L $bin ]] || die "No regular Chasm binary at $bin. For a checkout install, pass its --prefix."
    if [[ -w $(dirname "$bin") ]]; then rm -f "$bin"; else sudo rm -f "$bin"; fi
    printf 'Removed %s.\n' "$bin"
fi
printf 'Workspace data and wallets were not touched.\n'
