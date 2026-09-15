/* SPDX-License-Identifier: GPL-2.0 */
/*
 * Omnia OS kernel ABI.
 *
 * Shared verbatim between the kernel module, the BPF programs and the Python
 * userspace client (src/omnia/kernel/abi.py). `make check` parses this header
 * and asserts the Python struct definitions still match it -- a silent ABI
 * skew between a DKMS module and a package upgrade is the exact bug class this
 * file exists to make impossible.
 *
 * Compatibility rule: fields are only ever appended, never reordered or
 * resized, and OMNIA_ABI_VERSION is bumped when they are. Userspace refuses to
 * attach to a device whose ABI major differs.
 */

#ifndef _OMNIA_ABI_H
#define _OMNIA_ABI_H

#ifdef __KERNEL__
#include <linux/types.h>
#include <linux/ioctl.h>
#else
#include <linux/types.h>
#include <sys/ioctl.h>
#endif

#define OMNIA_ABI_VERSION	1u
#define OMNIA_DEVICE_NAME	"omnia"
#define OMNIA_COMM_LEN		16
#define OMNIA_PAYLOAD_LEN	192
#define OMNIA_RING_EVENTS	1024	/* power of two, kfifo requirement */

/* ---- event types -------------------------------------------------------- */
enum omnia_event_type {
	OMNIA_EV_NONE		= 0,
	OMNIA_EV_EXEC		= 1,	/* process exec, arg0 = ppid            */
	OMNIA_EV_OOM		= 2,	/* oom kill, arg0 = rss kb              */
	OMNIA_EV_UNIT_FAIL	= 3,	/* systemd unit entered failed          */
	OMNIA_EV_THERMAL	= 4,	/* arg0 = millidegrees, arg1 = trip     */
	OMNIA_EV_DISK_PRESSURE	= 5,	/* arg0 = free bytes, arg1 = total      */
	OMNIA_EV_SCHED_STALL	= 6,	/* arg0 = runqueue latency ns           */
	OMNIA_EV_NET_DOWN	= 7,	/* payload = ifname                     */
	OMNIA_EV_MEM_PRESSURE	= 8,	/* arg0 = PSI some avg10 (x100)         */
	OMNIA_EV_BLOCK_ERROR	= 9,	/* arg0 = errno, payload = device       */
	OMNIA_EV_GUARD_DENY	= 10,	/* the never-list refused something     */
	OMNIA_EV_WATCHDOG	= 11,	/* executor missed its heartbeat        */
	OMNIA_EV_USER		= 12,	/* emitted by userspace via OMNIA_IOC_EMIT */
	OMNIA_EV_MAX
};

#define OMNIA_EV_MASK(t)	(1ULL << (t))
#define OMNIA_EV_MASK_ALL	(~0ULL)

/* ---- severity ----------------------------------------------------------- */
enum omnia_severity {
	OMNIA_SEV_DEBUG	= 0,
	OMNIA_SEV_INFO	= 1,
	OMNIA_SEV_WARN	= 2,
	OMNIA_SEV_ERROR	= 3,
	OMNIA_SEV_CRIT	= 4,
};

/*
 * Fixed 256-byte record. Fixed size so a reader can mmap the ring and index
 * into it without a length prefix, and so a short read is always a bug rather
 * than a partial event.
 */
struct omnia_event {
	__u64 seq;			/* monotonic, gaps mean drops           */
	__u64 ts_ns;			/* CLOCK_MONOTONIC at emit              */
	__u32 type;			/* enum omnia_event_type                */
	__u32 pid;
	__u32 uid;
	__u32 severity;			/* enum omnia_severity                  */
	__u64 arg0;
	__u64 arg1;
	char  comm[OMNIA_COMM_LEN];
	char  payload[OMNIA_PAYLOAD_LEN];	/* NUL-terminated, type-specific */
};

/* ---- guard (never-list) ------------------------------------------------- */
enum omnia_guard_state {
	OMNIA_GUARD_ABSENT	= 0,	/* no BPF LSM programs pinned           */
	OMNIA_GUARD_ACTIVE	= 1,	/* enforcing                            */
	OMNIA_GUARD_DEGRADED	= 2,	/* pinned but a hook failed to attach   */
};

struct omnia_guard_status {
	__u32 state;			/* enum omnia_guard_state               */
	__u32 hooks_attached;
	__u64 denials;			/* cumulative since boot                */
	__u64 last_denial_ns;
	char  last_rule[OMNIA_COMM_LEN * 2];
};

struct omnia_stats {
	__u32 abi_version;
	__u32 readers;
	__u64 events_emitted;
	__u64 events_dropped;		/* ring full: the reader is too slow    */
	__u64 executor_pid;		/* 0 when no executor has claimed       */
	__u64 last_heartbeat_ns;
};

/* ---- ioctls ------------------------------------------------------------- */
#define OMNIA_IOC_MAGIC		'O'
#define OMNIA_IOC_ABI		_IOR(OMNIA_IOC_MAGIC, 0x01, __u32)
#define OMNIA_IOC_STATS		_IOR(OMNIA_IOC_MAGIC, 0x02, struct omnia_stats)
#define OMNIA_IOC_EMIT		_IOW(OMNIA_IOC_MAGIC, 0x03, struct omnia_event)
#define OMNIA_IOC_SUBSCRIBE	_IOW(OMNIA_IOC_MAGIC, 0x04, __u64)
#define OMNIA_IOC_GUARD		_IOR(OMNIA_IOC_MAGIC, 0x05, struct omnia_guard_status)
#define OMNIA_IOC_CLAIM		_IO(OMNIA_IOC_MAGIC,  0x06)
#define OMNIA_IOC_HEARTBEAT	_IO(OMNIA_IOC_MAGIC,  0x07)

#endif /* _OMNIA_ABI_H */
