// SPDX-License-Identifier: GPL-2.0
/*
 * Kernel telemetry for the autonomous loop.
 *
 * Only the signals that genuinely need to be in the kernel are here: events
 * that are edge-triggered, cheap to catch at the source, and expensive or racy
 * to reconstruct by polling from userspace.
 *
 *   exec         -- needed with the parent pid at fork time; /proc loses races
 *   oom kill     -- by the time userspace notices, the victim is already gone
 *   block error  -- the driver knows; /sys/block aggregates and forgets
 *   sigkill      -- who killed what, which /proc cannot tell you afterwards
 *
 * Everything else the loop consumes is a userspace poller instead, on purpose:
 * thermal zones, PSI, disk free, and link state are all level-triggered values
 * that a 1 Hz read of sysfs gets correctly, and a BPF program for them would be
 * more code, more attach surface, and more kernel-version fragility for no
 * signal. See omnia/kernel/pollers.py.
 */

#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

#include "omnia_bpf.h"

char LICENSE[] SEC("license") = "GPL";

struct {
	__uint(type, BPF_MAP_TYPE_RINGBUF);
	__uint(max_entries, 512 * 1024);
} exec_events SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_RINGBUF);
	__uint(max_entries, 256 * 1024);
} sys_events SEC(".maps");

/* Set by the loader. When 1, exec events are suppressed -- on an embedded board
 * a build or an apt run can produce thousands per second, and the loop does not
 * want them. The map lets omni autonomy tune it live without a reload. */
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(max_entries, 4);
	__type(key, __u32);
	__type(value, __u32);
} probe_config SEC(".maps");

#define CFG_MUTE_EXEC		0
#define CFG_MIN_OOM_RSS_KB	1

static __always_inline __u32 cfg(__u32 index)
{
	__u32 *value = bpf_map_lookup_elem(&probe_config, &index);

	return value ? *value : 0;
}

SEC("tracepoint/sched/sched_process_exec")
int omnia_trace_exec(struct trace_event_raw_sched_process_exec *ctx)
{
	struct omnia_exec_event *e;
	struct task_struct *task;
	unsigned int fname_off;

	if (cfg(CFG_MUTE_EXEC))
		return 0;

	e = bpf_ringbuf_reserve(&exec_events, sizeof(*e), 0);
	if (!e)
		return 0;	/* ring full: drop, the loop tolerates gaps */

	task = (struct task_struct *)bpf_get_current_task();

	e->ts_ns = bpf_ktime_get_ns();
	e->pid = bpf_get_current_pid_tgid() >> 32;
	e->ppid = BPF_CORE_READ(task, real_parent, tgid);
	e->uid = bpf_get_current_uid_gid();
	bpf_get_current_comm(&e->comm, sizeof(e->comm));

	/* The tracepoint stores the filename as a variable-offset string packed
	 * after the fixed fields; the offset is in the low 16 bits of
	 * __data_loc_filename. */
	fname_off = ctx->__data_loc_filename & 0xFFFF;
	bpf_probe_read_kernel_str(&e->filename, sizeof(e->filename),
				  (void *)ctx + fname_off);

	bpf_ringbuf_submit(e, 0);
	return 0;
}

SEC("tracepoint/oom/mark_victim")
int omnia_trace_oom(struct trace_event_raw_mark_victim *ctx)
{
	struct omnia_sys_event *e;
	struct task_struct *task;
	__u64 rss_kb;

	task = (struct task_struct *)bpf_get_current_task();
	rss_kb = (__u64)BPF_CORE_READ(task, mm, hiwater_rss) * 4;

	if (rss_kb < (__u64)cfg(CFG_MIN_OOM_RSS_KB))
		return 0;

	e = bpf_ringbuf_reserve(&sys_events, sizeof(*e), 0);
	if (!e)
		return 0;

	e->ts_ns = bpf_ktime_get_ns();
	e->kind = OMNIA_SYS_OOM;
	e->pid = BPF_CORE_READ(ctx, pid);
	e->arg0 = rss_kb;
	e->arg1 = 0;
	bpf_get_current_comm(&e->comm, sizeof(e->comm));
	e->detail[0] = '\0';

	bpf_ringbuf_submit(e, 0);
	return 0;
}

SEC("tracepoint/block/block_rq_error")
int omnia_trace_block_error(struct trace_event_raw_block_rq_error *ctx)
{
	struct omnia_sys_event *e;

	e = bpf_ringbuf_reserve(&sys_events, sizeof(*e), 0);
	if (!e)
		return 0;

	e->ts_ns = bpf_ktime_get_ns();
	e->kind = OMNIA_SYS_BLOCK_ERROR;
	e->pid = bpf_get_current_pid_tgid() >> 32;
	e->arg0 = BPF_CORE_READ(ctx, error);
	e->arg1 = BPF_CORE_READ(ctx, sector);
	bpf_get_current_comm(&e->comm, sizeof(e->comm));
	e->detail[0] = '\0';

	bpf_ringbuf_submit(e, 0);
	return 0;
}

SEC("tracepoint/signal/signal_generate")
int omnia_trace_signal(struct trace_event_raw_signal_generate *ctx)
{
	struct omnia_sys_event *e;
	int sig = BPF_CORE_READ(ctx, sig);

	/* SIGKILL only. Everything else is ordinary process lifecycle and would
	 * bury the interesting record. */
	if (sig != 9)
		return 0;

	e = bpf_ringbuf_reserve(&sys_events, sizeof(*e), 0);
	if (!e)
		return 0;

	e->ts_ns = bpf_ktime_get_ns();
	e->kind = OMNIA_SYS_SIGKILL;
	e->pid = bpf_get_current_pid_tgid() >> 32;	/* the sender */
	e->arg0 = BPF_CORE_READ(ctx, pid);		/* the target */
	e->arg1 = sig;
	bpf_get_current_comm(&e->comm, sizeof(e->comm));
	bpf_probe_read_kernel_str(&e->detail, sizeof(e->detail), ctx->comm);

	bpf_ringbuf_submit(e, 0);
	return 0;
}
