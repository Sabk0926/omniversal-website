#!/bin/sh
# Attach and pin the self-preservation floor. Runs from omnia-guard.service,
# before omniad, outside the guarded cgroup -- the loader must not be subject to
# the rules it installs, or it could not install them.
#
# Exits non-zero if the floor cannot be established. omniad then refuses to
# claim the executor role, and the system runs observe-only. That is the
# intended failure mode: no floor, no autonomy.
set -eu

BPFFS=/sys/fs/bpf
PIN=$BPFFS/omnia
OBJ=/usr/lib/omnia/bpf/omnia_guard.bpf.o
PROBES=/usr/lib/omnia/bpf/omnia_probes.bpf.o

log() { echo "omnia-guard: $*" >&2; }

if ! mountpoint -q "$BPFFS"; then
	mount -t bpf bpf "$BPFFS" || { log "no bpffs at $BPFFS"; exit 1; }
fi

if ! grep -qw bpf /sys/kernel/security/lsm 2>/dev/null; then
	log "BPF LSM is not enabled in this kernel (CONFIG_LSM must include 'bpf')"
	log "boot with lsm=...,bpf on the kernel command line to enable autonomy"
	exit 1
fi

rm -rf "$PIN"
mkdir -p "$PIN"

log "loading guard programs"
bpftool prog loadall "$OBJ" "$PIN/guard" autoattach

log "loading telemetry probes"
bpftool prog loadall "$PROBES" "$PIN/probes" autoattach

# Populate the maps: which cgroup is guarded, which device is the boot disk,
# which inodes are protected. Done in Python because it needs to resolve the
# systemd cgroup path, the boot device behind /, and stat a path list.
/usr/lib/omnia/omnia-guard-populate

chmod 0700 "$PIN"
log "floor active"
