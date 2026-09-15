// SPDX-License-Identifier: GPL-2.0
/*
 * Omnia self-preservation floor.
 *
 * Omnia runs fully autonomously: a root daemon acts on the system without
 * asking a human. The never-list therefore cannot live in that daemon. A rule
 * a process enforces on itself is one bad generation, one prompt injection or
 * one config edit away from not existing.
 *
 * These BPF LSM programs are attached and pinned at boot by omnia-guard.service
 * -- which runs BEFORE omniad and is not writable by it -- and enforce the
 * floor below the daemon. omniad refuses to claim the executor role if
 * OMNIA_IOC_GUARD does not report OMNIA_GUARD_ACTIVE, so the system cannot be
 * autonomous and unguarded at the same time.
 *
 * Scope is deliberately narrow. This is not a sandbox and does not try to be
 * one: it is the short list of actions from which there is no recovery on an
 * unattended machine.
 *
 *   1. Writing to the boot block device or its partition table
 *   2. Deleting the undo journal that makes every other action reversible
 *   3. Detaching or deleting the guard's own programs, links and maps
 *   4. Unmounting the root filesystem
 *   5. Touching the paths that keep a human able to get back in
 *      (sshd host keys, authorized_keys, sudoers, the console device)
 *
 * Everything else is the userspace budget/undo layer's problem.
 *
 * Requires CONFIG_BPF_LSM and "bpf" in CONFIG_LSM (Ubuntu ships both).
 */

#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

#include "omnia_bpf.h"

char LICENSE[] SEC("license") = "GPL";

#ifndef EPERM
#define EPERM 1
#endif

/* Open flags. Redefined locally: vmlinux.h does not carry uapi fcntl bits. */
#define OMNIA_O_ACCMODE	00000003
#define OMNIA_O_WRONLY	00000001
#define OMNIA_O_RDWR	00000002
#define OMNIA_O_TRUNC	00001000

#define S_IFMT_MASK	0170000
#define S_IFBLK_VAL	0060000

/* Cgroups subject to the floor. Populated by the loader with the cgroup ids of
 * omniad.service and anything it spawns (the service's own cgroup covers the
 * subtree, because cgroup ids of descendants are checked via ancestor walk). */
struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 16);
	__type(key, __u64);
	__type(value, __u8);
} guarded_cgroups SEC(".maps");

/* dev_t of block devices that must never be written by a guarded task. */
struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 32);
	__type(key, __u32);
	__type(value, __u8);
} protected_devs SEC(".maps");

/* (dev, ino) -> rule. Both the protected files themselves and the directories
 * whose children are protected. */
struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 512);
	__type(key, struct omnia_inode_key);
	__type(value, struct omnia_inode_rule);
} protected_inodes SEC(".maps");

/* Denials, drained by omnia.kernel.events and merged into the /dev/omnia
 * stream in userspace. They deliberately do not round-trip through the module's
 * OMNIA_IOC_EMIT, which rejects OMNIA_EV_GUARD_DENY: the one event type that
 * attests enforcement must not be forgeable by a userspace writer. */
struct {
	__uint(type, BPF_MAP_TYPE_RINGBUF);
	__uint(max_entries, 256 * 1024);
} guard_denials SEC(".maps");

/* Cumulative counters, read by omni doctor. Index = enum omnia_guard_rule. */
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(max_entries, OMNIA_RULE_MAX);
	__type(key, __u32);
	__type(value, __u64);
} guard_counters SEC(".maps");

static __always_inline int task_is_guarded(void)
{
	__u64 cgid = bpf_get_current_cgroup_id();

	if (bpf_map_lookup_elem(&guarded_cgroups, &cgid))
		return 1;

	/* Walk up: a helper the daemon forked lands in a child cgroup. Depth is
	 * bounded because the verifier requires it; six levels covers
	 * /sys/fs/cgroup/system.slice/omniad.service/<sub>. */
#pragma unroll
	for (int level = 1; level <= 6; level++) {
		__u64 ancestor = bpf_get_current_ancestor_cgroup_id(level);

		if (!ancestor)
			break;
		if (bpf_map_lookup_elem(&guarded_cgroups, &ancestor))
			return 1;
	}
	return 0;
}

static __always_inline void bump(__u32 rule)
{
	__u64 *count = bpf_map_lookup_elem(&guard_counters, &rule);

	if (count)
		__sync_fetch_and_add(count, 1);
}

static __always_inline int deny(__u32 rule, __u32 dev, __u64 ino,
				const char *target)
{
	struct omnia_guard_denial *d;

	bump(rule);

	d = bpf_ringbuf_reserve(&guard_denials, sizeof(*d), 0);
	if (d) {
		d->ts_ns = bpf_ktime_get_ns();
		d->cgroup_id = bpf_get_current_cgroup_id();
		d->pid = bpf_get_current_pid_tgid() >> 32;
		d->uid = bpf_get_current_uid_gid();
		d->rule = rule;
		d->dev = dev;
		d->ino = ino;
		bpf_get_current_comm(&d->comm, sizeof(d->comm));
		d->target[0] = '\0';
		if (target)
			bpf_probe_read_kernel_str(&d->target, sizeof(d->target), target);
		bpf_ringbuf_submit(d, 0);
	}

	return -EPERM;
}

/* ---- 1 & 5: opening the boot device, or a protected file, for write ------ */

SEC("lsm/file_open")
int BPF_PROG(omnia_file_open, struct file *file)
{
	struct inode *inode;
	struct super_block *sb;
	umode_t mode;
	__u32 dev;
	__u64 ino;
	unsigned int f_flags;
	struct omnia_inode_key key = {};
	struct omnia_inode_rule *rule;

	if (!task_is_guarded())
		return 0;

	inode = BPF_CORE_READ(file, f_inode);
	if (!inode)
		return 0;

	f_flags = BPF_CORE_READ(file, f_flags);
	if (!(f_flags & (OMNIA_O_WRONLY | OMNIA_O_RDWR | OMNIA_O_TRUNC)))
		return 0;	/* read-only opens are always fine */

	mode = BPF_CORE_READ(inode, i_mode);
	ino = BPF_CORE_READ(inode, i_ino);
	sb = BPF_CORE_READ(inode, i_sb);
	dev = BPF_CORE_READ(sb, s_dev);

	/* Rule 1: raw write to a protected block device. This is the one that
	 * turns a bad decision into a machine that does not boot. */
	if ((mode & S_IFMT_MASK) == S_IFBLK_VAL) {
		__u32 rdev = BPF_CORE_READ(inode, i_rdev);

		if (bpf_map_lookup_elem(&protected_devs, &rdev))
			return deny(OMNIA_RULE_BOOT_DEVICE, rdev, ino, NULL);
	}

	/* Rule 5: protected files (sshd keys, sudoers, console). */
	key.dev = dev;
	key.ino = ino;
	rule = bpf_map_lookup_elem(&protected_inodes, &key);
	if (rule && rule->deny_write)
		return deny(rule->rule, dev, ino, NULL);

	return 0;
}

/* ---- 2: deleting the undo journal --------------------------------------- */

static __always_inline int check_unlink(struct inode *dir, struct dentry *dentry)
{
	struct omnia_inode_key key = {};
	struct omnia_inode_rule *rule;
	struct inode *victim;
	__u32 dev;

	if (!task_is_guarded())
		return 0;

	/* Parent directory marked `recurse`: everything under it is protected,
	 * which is how the whole undo journal is covered by one entry. */
	dev = BPF_CORE_READ(dir, i_sb, s_dev);
	key.dev = dev;
	key.ino = BPF_CORE_READ(dir, i_ino);
	rule = bpf_map_lookup_elem(&protected_inodes, &key);
	if (rule && rule->recurse && rule->deny_unlink)
		return deny(rule->rule, dev, key.ino, NULL);

	/* Or the victim itself is individually protected. */
	victim = BPF_CORE_READ(dentry, d_inode);
	if (!victim)
		return 0;
	key.dev = BPF_CORE_READ(victim, i_sb, s_dev);
	key.ino = BPF_CORE_READ(victim, i_ino);
	rule = bpf_map_lookup_elem(&protected_inodes, &key);
	if (rule && rule->deny_unlink)
		return deny(rule->rule, key.dev, key.ino, NULL);

	return 0;
}

SEC("lsm/inode_unlink")
int BPF_PROG(omnia_inode_unlink, struct inode *dir, struct dentry *dentry)
{
	return check_unlink(dir, dentry);
}

SEC("lsm/inode_rmdir")
int BPF_PROG(omnia_inode_rmdir, struct inode *dir, struct dentry *dentry)
{
	return check_unlink(dir, dentry);
}

SEC("lsm/inode_rename")
int BPF_PROG(omnia_inode_rename, struct inode *old_dir, struct dentry *old_dentry,
	     struct inode *new_dir, struct dentry *new_dentry)
{
	/* Renaming the journal away is deletion with extra steps. */
	return check_unlink(old_dir, old_dentry);
}

/* ---- 3: the guard protecting itself ------------------------------------- */

SEC("lsm/bpf")
int BPF_PROG(omnia_bpf_cmd, int cmd, union bpf_attr *attr, unsigned int size)
{
	if (!task_is_guarded())
		return 0;

	/* A guarded task has no legitimate reason to detach programs, delete
	 * map elements or unpin links -- it consumes the guard's output, it
	 * does not administer it. The loader runs outside the guarded cgroup. */
	switch (cmd) {
	case BPF_PROG_DETACH:
	case BPF_LINK_DETACH:
	case BPF_MAP_DELETE_ELEM:
	case BPF_MAP_UPDATE_ELEM:
	case BPF_OBJ_PIN:
		return deny(OMNIA_RULE_GUARD_SELF, 0, 0, NULL);
	default:
		return 0;
	}
}

/* ---- 4: unmounting the root filesystem ---------------------------------- */

SEC("lsm/sb_umount")
int BPF_PROG(omnia_sb_umount, struct vfsmount *mnt, int flags)
{
	struct dentry *root, *sb_root;

	if (!task_is_guarded())
		return 0;

	root = BPF_CORE_READ(mnt, mnt_root);
	sb_root = BPF_CORE_READ(mnt, mnt_sb, s_root);
	if (root && root == sb_root) {
		__u32 dev = BPF_CORE_READ(mnt, mnt_sb, s_dev);
		struct omnia_inode_key key = {
			.dev = dev,
			.ino = BPF_CORE_READ(root, d_inode, i_ino),
		};

		if (bpf_map_lookup_elem(&protected_inodes, &key))
			return deny(OMNIA_RULE_ROOT_UMOUNT, dev, key.ino, NULL);
	}
	return 0;
}
