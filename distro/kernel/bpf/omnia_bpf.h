/* SPDX-License-Identifier: GPL-2.0 */
/* Types shared between the BPF programs and their userspace loader. */

#ifndef _OMNIA_BPF_H
#define _OMNIA_BPF_H

#define OMNIA_BPF_COMM_LEN	16
#define OMNIA_BPF_NAME_LEN	64
#define OMNIA_BPF_RULE_LEN	32

/* Rule identifiers. Kept numeric in the kernel and resolved to text in
 * userspace so the BPF side carries no strings it does not need. */
enum omnia_guard_rule {
	OMNIA_RULE_NONE			= 0,
	OMNIA_RULE_BOOT_DEVICE		= 1,	/* write to the boot block device   */
	OMNIA_RULE_UNDO_JOURNAL		= 2,	/* unlink under the undo journal    */
	OMNIA_RULE_GUARD_SELF		= 3,	/* detach/delete the guard itself   */
	OMNIA_RULE_ROOT_UMOUNT		= 4,	/* unmount the root filesystem      */
	OMNIA_RULE_ACCESS_PATH		= 5,	/* sshd keys, console, sudoers      */
	OMNIA_RULE_MAX
};

struct omnia_guard_denial {
	__u64 ts_ns;
	__u64 cgroup_id;
	__u32 pid;
	__u32 uid;
	__u32 rule;			/* enum omnia_guard_rule             */
	__u32 dev;
	__u64 ino;
	char  comm[OMNIA_BPF_COMM_LEN];
	char  target[OMNIA_BPF_NAME_LEN];
};

/* Key for the protected-inode map. Userspace stat()s each protected path at
 * load time and installs (dev, ino) pairs. Matching on inode rather than on a
 * reconstructed path is both cheaper and harder to defeat -- a symlink, a bind
 * mount or a mount namespace does not change the inode the kernel is about to
 * operate on. */
struct omnia_inode_key {
	__u64 ino;
	__u32 dev;
	__u32 _pad;
};

struct omnia_inode_rule {
	__u32 rule;		/* enum omnia_guard_rule                     */
	__u32 deny_write;	/* refuse write-ish opens                    */
	__u32 deny_unlink;	/* refuse unlink/rmdir/rename-over           */
	__u32 recurse;		/* also protect children (dir inode as parent) */
};

struct omnia_exec_event {
	__u64 ts_ns;
	__u32 pid;
	__u32 ppid;
	__u32 uid;
	char  comm[OMNIA_BPF_COMM_LEN];
	char  filename[OMNIA_BPF_NAME_LEN];
};

enum omnia_sys_kind {
	OMNIA_SYS_OOM		= 1,
	OMNIA_SYS_BLOCK_ERROR	= 2,
	OMNIA_SYS_SIGKILL	= 3,
};

struct omnia_sys_event {
	__u64 ts_ns;
	__u64 arg0;
	__u64 arg1;
	__u32 kind;		/* enum omnia_sys_kind                       */
	__u32 pid;
	char  comm[OMNIA_BPF_COMM_LEN];
	char  detail[OMNIA_BPF_NAME_LEN];
};

#endif /* _OMNIA_BPF_H */
