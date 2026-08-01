#!/bin/sh
set -eu

[ "$#" -eq 1 ] || {
    printf '%s\n' 'usage: test-install.sh PATH_TO_ASH' >&2
    exit 2
}

binary=$1
case $binary in
    /*) ;;
    *) binary=$PWD/$binary ;;
esac
script_directory=$(CDPATH= cd "$(dirname "$0")" && pwd -P)
repository_root=$(CDPATH= cd "$script_directory/.." && pwd -P)
installer=$repository_root/install.sh

fail() {
    printf 'installer smoke failure: %s\n' "$1" >&2
    exit 1
}

calculate_sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

expect_failure() {
    expected_code=$1
    shift
    set +e
    failure_output=$("$@" 2>&1)
    failure_status=$?
    set -e
    [ "$failure_status" -ne 0 ] || fail "command unexpectedly succeeded"
    printf '%s\n' "$failure_output" | grep -F -x "$expected_code" >/dev/null || {
        printf '%s\n' "$failure_output" >&2
        fail "missing stable error code $expected_code"
    }
}

temporary_base=$(CDPATH= cd "${TMPDIR:-/tmp}" && pwd -P)
temporary_root=$(mktemp -d "$temporary_base/ash-installer-smoke.XXXXXX") || exit 1
temporary_root=$(CDPATH= cd "$temporary_root" && pwd -P)
cleanup() {
    case $temporary_root in
        "$temporary_base"/ash-installer-smoke.*) rm -rf "$temporary_root" ;;
        *) fail 'unsafe temporary path' ;;
    esac
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

build_info=$($binary --build-info) || fail 'cannot read build metadata'
version=$(printf '%s\n' "$build_info" | sed -n 's/^v://p')
target=$(printf '%s\n' "$build_info" | sed -n 's/^t://p')
protocol=$(printf '%s\n' "$build_info" | sed -n 's/^p://p')
ason=$(printf '%s\n' "$build_info" | sed -n 's/^a://p')
case $target in
    x86_64-unknown-linux-musl|aarch64-unknown-linux-musl|x86_64-apple-darwin|aarch64-apple-darwin) ;;
    *) fail "unsupported smoke target $target" ;;
esac

package=$temporary_root/package
mkdir "$package"
cp "$binary" "$package/ash"
chmod 755 "$package/ash"
cp "$repository_root/LICENSE" "$package/LICENSE"
cp "$repository_root/THIRD-PARTY-LICENSES" "$package/THIRD-PARTY-LICENSES"
binary_sha=$(calculate_sha256 "$package/ash")
printf '{"schema":1,"product":"ash","version":"%s","target":"%s","protocol":"%s","ason":"%s","commit":"installer-smoke","build":"local","binary_sha256":"%s"}\n' \
    "$version" "$target" "$protocol" "$ason" "$binary_sha" > "$package/release.json"

archive=$temporary_root/ash-$target.tar.gz
tar -czf "$archive" -C "$package" LICENSE THIRD-PARTY-LICENSES ash release.json
archive_sha=$(calculate_sha256 "$archive")
unicode_suffix=$(printf '\342\230\203')
prefix=$temporary_root/prefix-$unicode_suffix-space
bin_dir=$temporary_root/bin-$unicode_suffix-space

fresh_output=$(sh "$installer" --archive "$archive" --sha256 "$archive_sha" --prefix "$prefix" --bin-dir "$bin_dir" --no-path)
printf '%s\n' "$fresh_output" | grep -F -x 's:0' >/dev/null || fail 'fresh install did not emit success ASON'
[ -x "$bin_dir/ash" ] || fail 'launcher was not installed'
"$bin_dir/ash" --build-info | grep -F -x "v:$version" >/dev/null || fail 'launcher version mismatch'
[ -f "$prefix/install-receipt.json" ] || fail 'receipt was not installed'

sh "$installer" --archive "$archive" --sha256 "$archive_sha" --prefix "$prefix" --bin-dir "$bin_dir" --no-path >/dev/null
sh "$installer" --archive "$archive" --sha256 "$archive_sha" --prefix "$prefix" --bin-dir "$bin_dir" --no-path --force >/dev/null
"$bin_dir/ash" --build-info | grep -F -x "t:$target" >/dev/null || fail 'forced reinstall changed target'

bad_prefix=$temporary_root/bad-checksum
bad_bin=$temporary_root/bad-checksum-bin
expect_failure 29 sh "$installer" --archive "$archive" --sha256 0000000000000000000000000000000000000000000000000000000000000000 --prefix "$bad_prefix" --bin-dir "$bad_bin" --no-path
[ ! -e "$bad_bin/ash" ] || fail 'checksum failure activated a binary'

extra_package=$temporary_root/extra-package
cp -R "$package" "$extra_package"
printf '%s\n' extra > "$extra_package/extra"
extra_archive=$temporary_root/extra.tar.gz
tar -czf "$extra_archive" -C "$extra_package" LICENSE THIRD-PARTY-LICENSES ash release.json extra
extra_sha=$(calculate_sha256 "$extra_archive")
expect_failure 31 sh "$installer" --archive "$extra_archive" --sha256 "$extra_sha" --prefix "$temporary_root/extra-prefix" --bin-dir "$temporary_root/extra-bin" --no-path
[ ! -e "$temporary_root/extra-bin/ash" ] || fail 'invalid archive shape activated a binary'

locked_prefix=$temporary_root/locked-prefix
mkdir -p "$locked_prefix/.install-lock"
expect_failure 14 sh "$installer" --archive "$archive" --sha256 "$archive_sha" --prefix "$locked_prefix" --bin-dir "$temporary_root/locked-bin" --no-path
[ -d "$locked_prefix/.install-lock" ] || fail 'contended installer removed another process lock'

conflict_prefix=$temporary_root/conflict-prefix
conflict_bin=$temporary_root/conflict-bin
mkdir -p "$conflict_bin"
printf '%s\n' sentinel > "$conflict_bin/ash"
expect_failure 39 sh "$installer" --archive "$archive" --sha256 "$archive_sha" --prefix "$conflict_prefix" --bin-dir "$conflict_bin" --no-path
[ "$(cat "$conflict_bin/ash")" = sentinel ] || fail 'rollback replaced an unowned launcher'
[ ! -d "$conflict_prefix/versions/$version" ] || fail 'rollback left a candidate version'

fake_home=$temporary_root/home
path_prefix=$temporary_root/path-prefix
path_bin=$temporary_root/path-bin
profile=$fake_home/profile
mkdir "$fake_home"
HOME=$fake_home ASH_INSTALL_PROFILE=$profile sh "$installer" --archive "$archive" --sha256 "$archive_sha" --prefix "$path_prefix" --bin-dir "$path_bin" >/dev/null
path_line="export PATH=\"$path_bin:\$PATH\" # ash-installer"
path_count=$(grep -F -x -c "$path_line" "$profile" || true)
if [ "$path_count" -ne 1 ]; then
    printf 'expected profile line: <%s>\n' "$path_line" >&2
    printf '%s\n' 'actual profile bytes:' >&2
    od -An -tx1 "$profile" >&2
    fail 'PATH line was not added exactly once'
fi
HOME=$fake_home ASH_INSTALL_PROFILE=$profile sh "$installer" --archive "$archive" --sha256 "$archive_sha" --prefix "$path_prefix" --bin-dir "$path_bin" >/dev/null
[ "$(grep -F -x -c "$path_line" "$profile")" -eq 1 ] || fail 'idempotent reinstall duplicated PATH'
HOME=$fake_home sh "$installer" --prefix "$path_prefix" --uninstall >/dev/null
[ "$(grep -F -x -c "$path_line" "$profile" || true)" -eq 0 ] || fail 'uninstall left an installer-owned PATH line'

removed_output=$(sh "$installer" --prefix "$prefix" --uninstall)
printf '%s\n' "$removed_output" | grep -F -x 's:0' >/dev/null || fail 'uninstall did not emit success ASON'
[ ! -e "$bin_dir/ash" ] || fail 'uninstall left the launcher'
[ ! -d "$prefix" ] || fail 'uninstall left the install root'

printf 's:0\na:installer-smoke-unix\n'
