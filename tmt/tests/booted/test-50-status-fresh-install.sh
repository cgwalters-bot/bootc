# number: 50
# extra:
#   fresh_install_disk: true
# tmt:
#   summary: Verify offline status paths survive a fresh installed boot
#   duration: 45m
#   adjust:
#     - when: status_fresh_install != true
#       enabled: false
#       because: this retains a VM and disk for runtime coordinator approval
#
# This is an offline status API test only. Bound images are explicitly skipped
# on both OSTree and composefs; native composefs fresh-install bound-image
# handling is a separate compatibility gap covered by test 22.
#
set -Eeuo pipefail

target=/var/mnt/bootc-status-fresh
source booted/fresh-install-state.sh

if test "${TMT_REBOOT_COUNT:-0}" != 0; then
    test "$(cat /etc/bootc-status-fresh-etc)" = "etc-${BOOTC_variant:-ostree}"
    test "$(cat /var/bootc-status-fresh-var)" = "var-${BOOTC_variant:-ostree}"
    booted=$(/usr/bin/bootc status --json)
    test "$(jq -r '.status.booted != null' <<<"$booted")" = true
    id=$(cat /etc/bootc-status-fresh-id)
    if test "${BOOTC_variant:-ostree}" = ostree; then
        test "$(jq -r '.status.booted.ostree != null' <<<"$booted")" = true
        test "$(jq -r '.status.booted.ostree | "\(.checksum).\(.deploySerial)"' <<<"$booted")" = "$id"
    else
        test "$(jq -r '.status.booted.composefs != null' <<<"$booted")" = true
        test "$(jq -r '.status.booted.composefs.verity' <<<"$booted")" = "$id"
        test -d "/sysroot/state/deploy/$id"
        findmnt -n -o SOURCE / | grep -Fx "composefs:$id"
    fi
    exit 0
fi
mkdir -p "$target"
snapshot_dir=$(mktemp -d /var/tmp/bootc-status-fresh.XXXXXX)
status="$snapshot_dir/status.json"
cleanup() {
    local rc=$?
    umount -R "$target" 2>/dev/null || true
    rm -rf "$snapshot_dir"
    return "$rc"
}
on_error() {
    local rc=$?
    trap - ERR
    printf 'fresh-install: ERROR line=%s command=%q\n' "${BASH_LINENO[0]:-$LINENO}" "$BASH_COMMAND" >&2
    cleanup
    exit "$rc"
}
trap on_error ERR
trap cleanup EXIT

# The VM disk attached by xtask is deliberately not the boot disk.  Install
# with the candidate image already booted by bcvk, never a host binary.
/usr/bin/bootc image copy-to-storage
printf '%s\n' 'fresh-install: bound images skipped; installing target disk'
# This follows the existing installer fixtures: disabling SELinux is required
# for this privileged, host-mount-namespace install. It does not claim to
# cover enforcing-label installation; this test covers offline state injection.
install_args=(--disable-selinux --bound-images=skip)
if test "${BOOTC_variant:-ostree}" = composefs; then
    bootloader=$(/usr/bin/bootc status --json | jq -er '.status.booted.composefs.bootloader')
    install_args+=(--composefs-backend --filesystem ext4 --bootloader "$bootloader")
fi
podman image inspect --format 'fresh-install candidate image={{.Id}}' localhost/bootc
podman run --pull=never --rm --privileged --pid=host --security-opt label=type:unconfined_t \
    -v /dev:/dev -v /var:/var localhost/bootc \
    /bin/sh -ec 'bootc --version; sha256sum /usr/bin/bootc; exec bootc install to-disk "$@"' \
    bootc-install "${install_args[@]}" /dev/vdb

# `bootc install to-disk` documents a root filesystem label; discover the
# resulting device from lsblk JSON instead of assuming a partition number.
root=$(lsblk --json --paths --output PATH,LABEL,TYPE /dev/vdb | jq -er \
    '[.. | objects | select(.type? == "part" and .label? == "root") | .path] | first')
test -n "$root"
mount "$root" "$target"
printf '%s\n' 'fresh-install: querying offline target status'

before="$snapshot_dir/before"
after="$snapshot_dir/after"
# Limit snapshots to state directories and boot metadata.  Metadata excludes
# atime; content hashes are limited to small metadata files, not image blobs.
snapshot() {
    fresh_install_snapshot "$target"
    fresh_install_snapshot_files /var/lib/tmt /var/tmp/tmt
}
# Record the complete mount namespace, not merely the root target mount: an
# offline query must not mount an ESP or another target filesystem elsewhere.
findmnt --json --all >"$before.mounts"
snapshot >"$before"
/usr/bin/bootc status --sysroot "$target" --json >"$status"
snapshot >"$after"
findmnt --json --all >"$after.mounts"
cmp "$before" "$after"
cmp "$before.mounts" "$after.mounts"

jq -e '.status.booted == null and .status.defaultDeployment.id != null' "$status" >/dev/null
backend=$(jq -r '.status.defaultDeployment.backend' "$status")
test "${BOOTC_variant:-ostree}" = "$backend"
etc=$(jq -r '.status.defaultDeployment.stateDirectories.etc' "$status")
var=$(jq -r '.status.defaultDeployment.stateDirectories.var' "$status")
id=$(jq -r '.status.defaultDeployment.id' "$status")
case "$etc" in "$target"/*) ;; *) exit 1;; esac
case "$var" in "$target"/*) ;; *) exit 1;; esac
test -d "$etc" && test -d "$var" && test "$etc" != "$var"
printf '%s\n' "etc-$backend" >"$etc/bootc-status-fresh-etc"
printf '%s\n' "var-$backend" >"$var/bootc-status-fresh-var"
printf '%s\n' "$id" >"$etc/bootc-status-fresh-id"
printf '%s\n' 'fresh-install: injected offline state sentinels'

# TMT's guest scripts default to /var/lib/tmt, while its execution run/workdir
# defaults to /var/tmp/tmt. Preserve both actual trees under the API-selected
# target /var before switching disks; neither path is under the target mount.
fresh_install_preserve_tmt_state "$var"

# bcvk's domain credential injection also supplies its reconnect key. Retain
# the guest's existing key in the installed state without printing its value.
if test -f /root/.ssh/authorized_keys; then
    install -D -m 0600 /root/.ssh/authorized_keys "$var/roothome/.ssh/authorized_keys"
fi
sync
umount -R "$target"
trap - EXIT ERR
cleanup
printf '%s\n' 'fresh-install: requesting runner-side fresh-disk boot'
tmt-reboot
