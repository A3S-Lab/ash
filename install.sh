#!/bin/sh
set -eu

repository="A3S-Lab/ash"
version=${ASH_INSTALL_VERSION:-}
channel=${ASH_INSTALL_CHANNEL:-stable}
prefix=${ASH_INSTALL_PREFIX:-${ASH_HOME:-${HOME:-}/.local/share/ash}}
bin_dir=${ASH_INSTALL_BIN_DIR:-${HOME:-}/.local/bin}
archive=${ASH_INSTALL_ARCHIVE:-}
expected_sha=${ASH_INSTALL_SHA256:-}
profile=${ASH_INSTALL_PROFILE:-}
no_path=0
force=0
uninstall=0
stage=""
lock=""
success=0
destination=""
destination_created=0
backup=""
active_changed=0
old_active=""
launcher=""
launcher_changed=0
old_launcher=""
path_owned=0
path_added_this_run=0
path_profile=""
path_line=""
receipt_path=""
receipt_existed=0
state_path=""
state_existed=0

fail() {
    code=$1
    printf 's:1\ne{c}:\n%s\n' "$code" >&2
    exit 1
}

usage() {
    printf '%s\n' 'usage: install.sh [--version V] [--channel stable] [--prefix PATH] [--bin-dir PATH] [--no-path] [--force] [--archive PATH --sha256 HEX] [--uninstall]' >&2
    exit 2
}

json_escape() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

ason_quote() {
    printf '"%s"' "$(json_escape "$1")"
}

shell_path_line() {
    escaped=$(printf '%s' "$bin_dir" | sed 's/[\\"`$]/\\&/g')
    printf 'export PATH="%s:$PATH" # ash-installer' "$escaped"
}

remove_profile_line() {
    file=$1
    line=$2
    [ -f "$file" ] || return 0
    temporary=$file.ash-remove.$$
    awk -v line="$line" '$0 != line { print }' "$file" > "$temporary" || {
        rm -f "$temporary"
        return 1
    }
    mv -f "$temporary" "$file"
}

restore_symlink() {
    link=$1
    target=$2
    temporary=$link.ash-rollback.$$
    rm -f "$temporary"
    ln -s "$target" "$temporary" && mv -f "$temporary" "$link"
}

rollback() {
    if [ "$path_added_this_run" -eq 1 ] && [ -n "$path_profile" ] && [ -n "$path_line" ]; then
        remove_profile_line "$path_profile" "$path_line" || true
    fi
    if [ "$launcher_changed" -eq 1 ] && [ -n "$launcher" ]; then
        if [ -n "$old_launcher" ]; then
            restore_symlink "$launcher" "$old_launcher" || true
        else
            rm -f "$launcher"
        fi
    fi
    if [ "$destination_created" -eq 1 ] && [ -n "$destination" ] && [ -d "$destination" ]; then
        rm -rf "$destination"
    fi
    if [ -n "$backup" ] && [ -d "$backup" ] && [ -n "$destination" ]; then
        rm -rf "$destination"
        mv "$backup" "$destination" || true
    fi
    if [ "$active_changed" -eq 1 ]; then
        if [ -n "$old_active" ]; then
            restore_symlink "$prefix/active" "$old_active" || true
        else
            rm -f "$prefix/active"
        fi
    fi
    if [ -n "$receipt_path" ]; then
        if [ "$receipt_existed" -eq 1 ] && [ -f "$stage/receipt.backup" ]; then
            mv -f "$stage/receipt.backup" "$receipt_path" || true
        elif [ "$receipt_existed" -eq 0 ]; then
            rm -f "$receipt_path"
        fi
    fi
    if [ -n "$state_path" ]; then
        if [ "$state_existed" -eq 1 ] && [ -f "$stage/state.backup" ]; then
            mv -f "$stage/state.backup" "$state_path" || true
        elif [ "$state_existed" -eq 0 ]; then
            rm -f "$state_path"
        fi
    fi
}

cleanup() {
    status=$?
    trap - EXIT
    if [ "$status" -ne 0 ] && [ "$success" -eq 0 ] && [ -n "$stage" ]; then
        rollback
    fi
    if [ -n "$stage" ] && [ -d "$stage" ]; then
        rm -rf "$stage"
    fi
    if [ -n "$lock" ] && [ -d "$lock" ]; then
        rm -f "$lock/owner"
        rmdir "$lock" 2>/dev/null || true
    fi
    exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

while [ "$#" -gt 0 ]; do
    case $1 in
        --version) [ "$#" -ge 2 ] || usage; version=$2; shift 2 ;;
        --channel) [ "$#" -ge 2 ] || usage; channel=$2; shift 2 ;;
        --prefix) [ "$#" -ge 2 ] || usage; prefix=$2; shift 2 ;;
        --bin-dir) [ "$#" -ge 2 ] || usage; bin_dir=$2; shift 2 ;;
        --archive) [ "$#" -ge 2 ] || usage; archive=$2; shift 2 ;;
        --sha256) [ "$#" -ge 2 ] || usage; expected_sha=$2; shift 2 ;;
        --no-path) no_path=1; shift ;;
        --force) force=1; shift ;;
        --uninstall) uninstall=1; shift ;;
        -h|--help) usage ;;
        *) usage ;;
    esac
done

[ -n "${HOME:-}" ] || fail 11
[ "$channel" = stable ] || fail 12
version=${version#v}
case $version in
    *[!0-9A-Za-z.+-]*) fail 12 ;;
esac

absolute_path() {
    value=$1
    case $value in
        /*) ;;
        *) value=$PWD/$value ;;
    esac
    parent=${value%/*}
    name=${value##*/}
    case $name in ''|.|..) fail 13 ;; esac
    mkdir -p "$parent"
    parent=$(CDPATH= cd "$parent" && pwd -P)
    printf '%s/%s\n' "$parent" "$name"
}

prefix=$(absolute_path "$prefix")
bin_dir=$(absolute_path "$bin_dir")
case $prefix in
    /|"$HOME"|"$bin_dir") fail 13 ;;
esac
newline='
'
carriage_return=$(printf '\rX')
carriage_return=${carriage_return%X}
tab=$(printf '\tX')
tab=${tab%X}
case "$prefix$bin_dir$profile" in
    *"$newline"*|*"$carriage_return"*|*"$tab"*) fail 13 ;;
esac

mkdir -p "$prefix"
lock_candidate=$prefix/.install-lock
mkdir "$lock_candidate" 2>/dev/null || fail 14
lock=$lock_candidate
printf '%s\n' "$$" > "$lock/owner" || fail 14

receipt_path=$prefix/install-receipt.json
state_path=$prefix/install-state

if [ "$uninstall" -eq 1 ]; then
    [ -f "$receipt_path" ] || fail 16
    escaped_prefix=$(json_escape "$prefix")
    grep -F '"schema":1' "$receipt_path" >/dev/null || fail 16
    grep -F '"repository":"A3S-Lab/ash"' "$receipt_path" >/dev/null || fail 16
    grep -F "\"prefix\":\"$escaped_prefix\"" "$receipt_path" >/dev/null || fail 16
    if [ -f "$state_path" ]; then
        [ "$(sed -n '1p' "$state_path")" = 1 ] || fail 16
        recorded_bin=$(sed -n '2p' "$state_path")
        recorded_profile=$(sed -n '3p' "$state_path")
        [ "$(wc -l < "$state_path" | tr -d ' ')" -eq 3 ] || fail 16
        case $recorded_bin in /*) ;; *) fail 16 ;; esac
        [ "$recorded_bin" != / ] || fail 16
        [ -n "$recorded_bin" ] && bin_dir=$recorded_bin
        if grep -F '"path_added":true' "$receipt_path" >/dev/null && [ -n "$recorded_profile" ]; then
            path_profile=$recorded_profile
            path_line=$(shell_path_line)
            remove_profile_line "$path_profile" "$path_line" || fail 15
        fi
    elif grep -F '"path_added":true' "$receipt_path" >/dev/null; then
        fail 16
    fi
    launcher=$bin_dir/ash
    escaped_launcher=$(json_escape "$launcher")
    grep -F "\"launcher\":\"$escaped_launcher\"" "$receipt_path" >/dev/null || fail 16
    if [ -L "$launcher" ]; then
        target=$(readlink "$launcher")
        [ "$target" = "$prefix/active/ash" ] || fail 16
        rm -f "$launcher"
    elif [ -e "$launcher" ]; then
        fail 16
    fi
    rm -f "$prefix/active" "$receipt_path" "$state_path"
    if [ -d "$prefix/versions" ]; then
        rm -rf "$prefix/versions"
    fi
    rm -f "$lock/owner"
    rmdir "$lock" 2>/dev/null || true
    lock=""
    rmdir "$prefix" 2>/dev/null || true
    printf 's:0\na:uninstalled\np:%s\n' "$(ason_quote "$prefix")"
    success=1
    exit 0
fi

case $(uname -s 2>/dev/null || true) in
    Linux) os=linux ;;
    Darwin) os=darwin ;;
    *) fail 20 ;;
esac
machine=$(uname -m 2>/dev/null || true)
if [ "$os" = darwin ] && [ "$machine" = x86_64 ] && command -v sysctl >/dev/null 2>&1; then
    translated=$(sysctl -in sysctl.proc_translated 2>/dev/null || printf '0')
    [ "$translated" = 1 ] && machine=arm64
fi
case "$os:$machine" in
    linux:x86_64|linux:amd64) target=x86_64-unknown-linux-musl ;;
    linux:aarch64|linux:arm64) target=aarch64-unknown-linux-musl ;;
    darwin:x86_64) target=x86_64-apple-darwin ;;
    darwin:arm64|darwin:aarch64) target=aarch64-apple-darwin ;;
    *) fail 21 ;;
esac
asset=ash-$target.tar.gz

stage=$(mktemp -d "${TMPDIR:-/tmp}/ash-install.XXXXXX") || fail 22
download() {
    source_url=$1
    destination_path=$2
    if command -v curl >/dev/null 2>&1; then
        curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
            --output "$destination_path" "$source_url" || fail 23
    elif command -v wget >/dev/null 2>&1; then
        wget --https-only --quiet --output-document="$destination_path" "$source_url" || fail 23
    else
        fail 24
    fi
}

online=0
if [ -n "$archive" ]; then
    [ -f "$archive" ] || fail 25
    [ -n "$expected_sha" ] || fail 26
    cp "$archive" "$stage/$asset"
else
    online=1
    if [ -n "$version" ]; then
        base=https://github.com/$repository/releases/download/v$version
    else
        base=https://github.com/$repository/releases/latest/download
    fi
    download "$base/$asset" "$stage/$asset"
    download "$base/SHA256SUMS" "$stage/SHA256SUMS"
    expected_sha=$(awk -v asset="$asset" '$2 == asset || $2 == "*" asset { print $1; exit }' "$stage/SHA256SUMS")
    [ -n "$expected_sha" ] || fail 27
fi

calculate_sha256() {
    file=$1
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | awk '{print $1}'
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 "$file" | awk '{print $NF}'
    else
        fail 28
    fi
}

actual_sha=$(calculate_sha256 "$stage/$asset" | tr 'A-F' 'a-f')
expected_sha=$(printf '%s' "$expected_sha" | tr 'A-F' 'a-f')
case $expected_sha in
    *[!0-9a-f]*|'') fail 26 ;;
esac
[ "${#expected_sha}" -eq 64 ] || fail 26
[ "$actual_sha" = "$expected_sha" ] || fail 29

archive_entries=$(tar -tzf "$stage/$asset" 2>/dev/null | LC_ALL=C sort) || fail 30
expected_entries=$(printf '%s\n' LICENSE THIRD-PARTY-LICENSES ash release.json | LC_ALL=C sort)
[ "$archive_entries" = "$expected_entries" ] || fail 31

extract=$stage/extract
mkdir "$extract"
tar -xzf "$stage/$asset" -C "$extract" || fail 30
for name in ash LICENSE THIRD-PARTY-LICENSES release.json; do
    [ -f "$extract/$name" ] && [ ! -L "$extract/$name" ] || fail 31
done
chmod 755 "$extract/ash"

if [ "$online" -eq 1 ] && [ "$os" = darwin ]; then
    command -v codesign >/dev/null 2>&1 || fail 35
    codesign --verify --strict "$extract/ash" >/dev/null 2>&1 || fail 35
fi

build_info=$($extract/ash --build-info 2>/dev/null) || fail 32
actual_version=$(printf '%s\n' "$build_info" | sed -n 's/^v://p')
actual_target=$(printf '%s\n' "$build_info" | sed -n 's/^t://p')
protocol_version=$(printf '%s\n' "$build_info" | sed -n 's/^p://p')
ason_version=$(printf '%s\n' "$build_info" | sed -n 's/^a://p')
case $actual_version in
    *[!0-9A-Za-z.+-]*|'') fail 32 ;;
esac
[ "$actual_target" = "$target" ] || fail 33
[ "$protocol_version" = 1 ] && [ "$ason_version" = 1 ] || fail 32
[ -z "$version" ] || [ "$actual_version" = "$version" ] || fail 34
version=$actual_version
binary_sha=$(calculate_sha256 "$extract/ash" | tr 'A-F' 'a-f')
grep -F '"schema":1' "$extract/release.json" >/dev/null || fail 31
grep -F '"product":"ash"' "$extract/release.json" >/dev/null || fail 31
grep -F "\"version\":\"$version\"" "$extract/release.json" >/dev/null || fail 34
grep -F "\"target\":\"$target\"" "$extract/release.json" >/dev/null || fail 33
grep -F "\"binary_sha256\":\"$binary_sha\"" "$extract/release.json" >/dev/null || fail 29

versions=$prefix/versions
mkdir -p "$versions"
destination=$versions/$version
candidate=$versions/.candidate-$version-$$
if [ -e "$destination" ]; then
    [ -d "$destination" ] && [ ! -L "$destination" ] || fail 36
    if [ "$force" -eq 0 ]; then
        existing=$($destination/ash --build-info 2>/dev/null || true)
        printf '%s\n' "$existing" | grep -F -x "v:$version" >/dev/null || fail 36
        printf '%s\n' "$existing" | grep -F -x "t:$target" >/dev/null || fail 36
        existing_sha=$(calculate_sha256 "$destination/ash" | tr 'A-F' 'a-f')
        [ "$existing_sha" = "$binary_sha" ] || fail 36
    else
        backup=$versions/.backup-$version-$$
        mv "$destination" "$backup" || fail 37
    fi
fi
if [ ! -e "$destination" ]; then
    mkdir "$candidate"
    cp "$extract/ash" "$candidate/ash"
    cp "$extract/LICENSE" "$candidate/LICENSE"
    cp "$extract/THIRD-PARTY-LICENSES" "$candidate/THIRD-PARTY-LICENSES"
    cp "$extract/release.json" "$candidate/release.json"
    mv "$candidate" "$destination" || fail 37
    destination_created=1
fi

if [ -e "$prefix/active" ] || [ -L "$prefix/active" ]; then
    [ -L "$prefix/active" ] || fail 38
    old_active=$(readlink "$prefix/active")
fi
mkdir -p "$bin_dir"
launcher=$bin_dir/ash
if [ -e "$launcher" ] || [ -L "$launcher" ]; then
    [ -L "$launcher" ] || fail 39
    old_launcher=$(readlink "$launcher")
fi
if [ -f "$receipt_path" ]; then
    cp "$receipt_path" "$stage/receipt.backup"
    receipt_existed=1
fi
if [ -f "$state_path" ]; then
    cp "$state_path" "$stage/state.backup"
    state_existed=1
fi

if [ "$receipt_existed" -eq 1 ] && grep -F '"path_added":true' "$receipt_path" >/dev/null; then
    [ "$state_existed" -eq 1 ] || fail 16
    [ "$(sed -n '1p' "$state_path")" = 1 ] || fail 16
    prior_bin=$(sed -n '2p' "$state_path")
    prior_profile=$(sed -n '3p' "$state_path")
    [ "$(wc -l < "$state_path" | tr -d ' ')" -eq 3 ] || fail 16
    [ "$prior_bin" = "$bin_dir" ] || fail 36
    [ -n "$prior_profile" ] || fail 16
    path_owned=1
    path_profile=$prior_profile
    path_line=$(shell_path_line)
fi

active_new=$prefix/.active-$$
ln -s "versions/$version" "$active_new" || fail 38
mv -f "$active_new" "$prefix/active" || fail 38
active_changed=1

launcher_new=$bin_dir/.ash-$$
ln -s "$prefix/active/ash" "$launcher_new" || fail 39
mv -f "$launcher_new" "$launcher" || fail 39
launcher_changed=1

active_info=$($launcher --build-info 2>/dev/null) || fail 40
printf '%s\n' "$active_info" | grep -F -x "v:$version" >/dev/null || fail 40
printf '%s\n' "$active_info" | grep -F -x "t:$target" >/dev/null || fail 40

case :$PATH: in
    *:"$bin_dir":*) ;;
    *)
        if [ "$no_path" -eq 0 ]; then
            if [ -z "$profile" ]; then
                case ${SHELL##*/} in
                    zsh) profile=$HOME/.zshrc ;;
                    bash) profile=$HOME/.bashrc ;;
                    *) profile=$HOME/.profile ;;
                esac
            fi
            path_line=$(shell_path_line)
            touch "$profile" || fail 41
            if ! grep -F -x "$path_line" "$profile" >/dev/null 2>&1; then
                printf '\n%s\n' "$path_line" >> "$profile" || fail 41
                path_owned=1
                path_added_this_run=1
                path_profile=$profile
            fi
        fi
        ;;
esac

state_tmp=$prefix/.install-state-$$
printf '1\n%s\n%s\n' "$bin_dir" "$path_profile" > "$state_tmp" || fail 42
mv -f "$state_tmp" "$state_path" || fail 42

if [ "$path_owned" -eq 1 ]; then
    path_added_json=true
else
    path_added_json=false
fi
receipt_tmp=$prefix/.install-receipt-$$
printf '{"schema":1,"repository":"%s","version":"%s","target":"%s","prefix":"%s","launcher":"%s","path_added":%s,"profile":"%s"}\n' \
    "$repository" "$version" "$target" "$(json_escape "$prefix")" "$(json_escape "$launcher")" \
    "$path_added_json" "$(json_escape "$path_profile")" > "$receipt_tmp" || fail 42
mv -f "$receipt_tmp" "$receipt_path" || fail 42

[ -z "$backup" ] || rm -rf "$backup"
backup=""
success=1
printf 's:0\na:installed\nv:%s\nt:%s\np:%s\n' "$version" "$target" "$(ason_quote "$launcher")"
