// SPDX-License-Identifier: GPL-2.0
/*
 * Omnia OS character device.
 *
 * What this module is for, and deliberately what it is not:
 *
 *   IS   a stable, always-present rendezvous point (/dev/omnia) that exists
 *        from initramfs onward, a lossy-but-accounted event ring that kernel
 *        sources push into without a userspace poller, and the bookkeeping for
 *        exactly one "executor" process plus its heartbeat.
 *
 *   IS NOT an inference engine. No floating point, no large allocations, no
 *        model weights. Tensors belong in userspace where a bug is a core dump
 *        rather than a panic. Enforcement of the never-list lives in the BPF
 *        LSM programs (see ../bpf/omnia_guard.bpf.c); this module only reports
 *        their status so userspace can refuse to run autonomously without them.
 *
 * The executor claim matters because the system is autonomous: exactly one
 * process may hold the executor role, the kernel records its pid, and the
 * watchdog fires an event if it stops heart-beating. A wedged autonomous daemon
 * is more dangerous than an absent one, so its absence must be observable.
 */

#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/init.h>
#include <linux/fs.h>
#include <linux/miscdevice.h>
#include <linux/uaccess.h>
#include <linux/kfifo.h>
#include <linux/poll.h>
#include <linux/sched.h>
#include <linux/slab.h>
#include <linux/mutex.h>
#include <linux/timer.h>
#include <linux/jiffies.h>
#include <linux/pid.h>
#include <linux/cred.h>
#include <linux/version.h>

#include "omnia_abi.h"

#define OMNIA_WATCHDOG_SECS	30

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Omnia OS");
MODULE_DESCRIPTION("Omnia OS event ring and executor rendezvous device");
MODULE_VERSION("0.1.0");

static unsigned int ring_events = OMNIA_RING_EVENTS;
module_param(ring_events, uint, 0444);
MODULE_PARM_DESC(ring_events, "event ring capacity in records (power of two)");

static bool watchdog_enabled = true;
module_param(watchdog_enabled, bool, 0644);
MODULE_PARM_DESC(watchdog_enabled, "emit OMNIA_EV_WATCHDOG when the executor stalls");

/* ------------------------------------------------------------------------ */

struct omnia_dev {
	struct kfifo		fifo;		/* of struct omnia_event      */
	spinlock_t		fifo_lock;
	wait_queue_head_t	readers;
	struct mutex		ctl_lock;	/* serialises ioctl mutations */

	atomic64_t		seq;
	atomic64_t		emitted;
	atomic64_t		dropped;
	atomic_t		reader_count;

	struct pid		*executor;
	u64			last_heartbeat_ns;

	struct omnia_guard_status guard;
	struct timer_list	watchdog;
};

static struct omnia_dev *omnia;

/* Per-open state. Each reader gets its own type mask so a shell tailing exec
 * events does not have to filter out thermal noise in userspace. */
struct omnia_file {
	u64 mask;
};

static inline u64 omnia_now_ns(void)
{
	return ktime_get_ns();
}

/* ------------------------------------------------------------------------ */
/* Emit path. Callable from any context, including softirq: kfifo_in under a
 * spinlock, no allocation, no sleeping. Drops are counted, never blocked on --
 * a slow reader must not be able to stall a kernel event source. */

static void omnia_emit(struct omnia_event *ev)
{
	unsigned long flags;
	unsigned int copied;

	ev->seq = atomic64_inc_return(&omnia->seq);
	if (!ev->ts_ns)
		ev->ts_ns = omnia_now_ns();

	spin_lock_irqsave(&omnia->fifo_lock, flags);
	if (kfifo_avail(&omnia->fifo) < sizeof(*ev)) {
		struct omnia_event discard;

		/* Ring full: drop the OLDEST record. During an incident the
		 * newest events are the ones that explain it; a reader that
		 * fell behind wants the tail, not a stale head. The gap is
		 * visible either way because seq numbers skip. */
		kfifo_out(&omnia->fifo, &discard, sizeof(discard));
		atomic64_inc(&omnia->dropped);
	}
	copied = kfifo_in(&omnia->fifo, ev, sizeof(*ev));
	spin_unlock_irqrestore(&omnia->fifo_lock, flags);

	if (copied == sizeof(*ev)) {
		atomic64_inc(&omnia->emitted);
		wake_up_interruptible(&omnia->readers);
	} else {
		atomic64_inc(&omnia->dropped);
	}
}

/* Exported so sibling in-tree helpers (and the guard's ring drain) can post
 * events without going through userspace. */
void omnia_post_event(u32 type, u32 severity, u64 arg0, u64 arg1,
		      const char *payload)
{
	struct omnia_event ev;

	if (!omnia || type >= OMNIA_EV_MAX)
		return;

	memset(&ev, 0, sizeof(ev));
	ev.type = type;
	ev.severity = severity;
	ev.arg0 = arg0;
	ev.arg1 = arg1;
	ev.pid = current->pid;
	ev.uid = from_kuid(&init_user_ns, current_uid());
	memcpy(ev.comm, current->comm, min_t(size_t, sizeof(ev.comm) - 1,
					     sizeof(current->comm)));
	if (payload)
		strscpy(ev.payload, payload, sizeof(ev.payload));

	omnia_emit(&ev);
}
EXPORT_SYMBOL_GPL(omnia_post_event);

/* ------------------------------------------------------------------------ */
/* Watchdog: if an executor has claimed the role and then stops heart-beating,
 * say so loudly. Autonomy that has silently stopped looks identical to a
 * healthy idle system, which is the failure we refuse to allow. */

static void omnia_watchdog_fn(struct timer_list *t)
{
	struct omnia_dev *dev = from_timer(dev, t, watchdog);
	u64 now = omnia_now_ns();
	bool stalled;

	stalled = dev->executor && dev->last_heartbeat_ns &&
		  (now - dev->last_heartbeat_ns) >
			  (u64)OMNIA_WATCHDOG_SECS * 2 * NSEC_PER_SEC;

	if (stalled && watchdog_enabled) {
		struct omnia_event ev;

		memset(&ev, 0, sizeof(ev));
		ev.type = OMNIA_EV_WATCHDOG;
		ev.severity = OMNIA_SEV_CRIT;
		ev.pid = pid_vnr(dev->executor);
		ev.arg0 = (now - dev->last_heartbeat_ns) / NSEC_PER_SEC;
		strscpy(ev.payload, "executor heartbeat expired",
			sizeof(ev.payload));
		omnia_emit(&ev);
	}

	mod_timer(&dev->watchdog,
		  jiffies + msecs_to_jiffies(OMNIA_WATCHDOG_SECS * 1000));
}

/* ------------------------------------------------------------------------ */
/* File operations. */

static int omnia_open(struct inode *inode, struct file *file)
{
	struct omnia_file *priv;

	priv = kzalloc(sizeof(*priv), GFP_KERNEL);
	if (!priv)
		return -ENOMEM;

	priv->mask = OMNIA_EV_MASK_ALL;
	file->private_data = priv;
	atomic_inc(&omnia->reader_count);
	return 0;
}

static int omnia_release(struct inode *inode, struct file *file)
{
	mutex_lock(&omnia->ctl_lock);
	if (omnia->executor && pid_vnr(omnia->executor) == current->tgid) {
		/* The executor went away. Drop the claim so a replacement can
		 * take over, and make the transition auditable. */
		put_pid(omnia->executor);
		omnia->executor = NULL;
		omnia->last_heartbeat_ns = 0;
		omnia_post_event(OMNIA_EV_WATCHDOG, OMNIA_SEV_WARN, 0, 0,
				 "executor released the claim");
	}
	mutex_unlock(&omnia->ctl_lock);

	atomic_dec(&omnia->reader_count);
	kfree(file->private_data);
	return 0;
}

static ssize_t omnia_read(struct file *file, char __user *buf, size_t count,
			  loff_t *ppos)
{
	struct omnia_file *priv = file->private_data;
	struct omnia_event ev;
	unsigned long flags;
	size_t written = 0;
	int ret;

	/* Fixed-size records: a buffer smaller than one event can never make
	 * progress, so reject it rather than spin. */
	if (count < sizeof(ev))
		return -EINVAL;

	for (;;) {
		unsigned int got;

		spin_lock_irqsave(&omnia->fifo_lock, flags);
		got = kfifo_out(&omnia->fifo, &ev, sizeof(ev));
		spin_unlock_irqrestore(&omnia->fifo_lock, flags);

		if (!got) {
			if (written)
				break;
			if (file->f_flags & O_NONBLOCK)
				return -EAGAIN;
			ret = wait_event_interruptible(omnia->readers,
						       !kfifo_is_empty(&omnia->fifo));
			if (ret)
				return ret;
			continue;
		}

		if (!(priv->mask & OMNIA_EV_MASK(ev.type)))
			continue;	/* filtered out for this reader */

		if (copy_to_user(buf + written, &ev, sizeof(ev)))
			return written ? (ssize_t)written : -EFAULT;

		written += sizeof(ev);
		if (written + sizeof(ev) > count)
			break;
	}

	return written;
}

static __poll_t omnia_poll(struct file *file, poll_table *wait)
{
	poll_wait(file, &omnia->readers, wait);
	return kfifo_is_empty(&omnia->fifo) ? 0 : (EPOLLIN | EPOLLRDNORM);
}

static long omnia_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	struct omnia_file *priv = file->private_data;
	void __user *uarg = (void __user *)arg;
	long ret = 0;

	switch (cmd) {
	case OMNIA_IOC_ABI: {
		u32 version = OMNIA_ABI_VERSION;

		if (copy_to_user(uarg, &version, sizeof(version)))
			return -EFAULT;
		return 0;
	}

	case OMNIA_IOC_STATS: {
		struct omnia_stats stats;

		memset(&stats, 0, sizeof(stats));
		stats.abi_version = OMNIA_ABI_VERSION;
		stats.readers = atomic_read(&omnia->reader_count);
		stats.events_emitted = atomic64_read(&omnia->emitted);
		stats.events_dropped = atomic64_read(&omnia->dropped);

		mutex_lock(&omnia->ctl_lock);
		stats.executor_pid = omnia->executor ? pid_vnr(omnia->executor) : 0;
		stats.last_heartbeat_ns = omnia->last_heartbeat_ns;
		mutex_unlock(&omnia->ctl_lock);

		if (copy_to_user(uarg, &stats, sizeof(stats)))
			return -EFAULT;
		return 0;
	}

	case OMNIA_IOC_EMIT: {
		struct omnia_event ev;

		/* Userspace may inject events (the BPF ring drain and the
		 * systemd bridge both do). It may not forge provenance: pid,
		 * uid and comm are overwritten with the caller's real identity,
		 * and it cannot claim a guard denial. */
		if (!capable(CAP_SYS_ADMIN))
			return -EPERM;
		if (copy_from_user(&ev, uarg, sizeof(ev)))
			return -EFAULT;
		if (ev.type >= OMNIA_EV_MAX || ev.type == OMNIA_EV_GUARD_DENY)
			return -EINVAL;

		ev.pid = current->pid;
		ev.uid = from_kuid(&init_user_ns, current_uid());
		memcpy(ev.comm, current->comm, min_t(size_t, sizeof(ev.comm) - 1,
						     sizeof(current->comm)));
		ev.payload[sizeof(ev.payload) - 1] = '\0';
		ev.ts_ns = 0;
		omnia_emit(&ev);
		return 0;
	}

	case OMNIA_IOC_SUBSCRIBE: {
		u64 mask;

		if (copy_from_user(&mask, uarg, sizeof(mask)))
			return -EFAULT;
		priv->mask = mask;
		return 0;
	}

	case OMNIA_IOC_GUARD: {
		struct omnia_guard_status status;

		mutex_lock(&omnia->ctl_lock);
		status = omnia->guard;
		mutex_unlock(&omnia->ctl_lock);

		if (copy_to_user(uarg, &status, sizeof(status)))
			return -EFAULT;
		return 0;
	}

	case OMNIA_IOC_CLAIM: {
		if (!capable(CAP_SYS_ADMIN))
			return -EPERM;

		mutex_lock(&omnia->ctl_lock);
		if (omnia->executor && pid_vnr(omnia->executor) != current->tgid) {
			/* Two autonomous executors on one machine would fight
			 * over the same system. One is a design invariant. */
			ret = -EBUSY;
		} else {
			if (!omnia->executor)
				omnia->executor = get_pid(task_tgid(current));
			omnia->last_heartbeat_ns = omnia_now_ns();
		}
		mutex_unlock(&omnia->ctl_lock);
		return ret;
	}

	case OMNIA_IOC_HEARTBEAT: {
		mutex_lock(&omnia->ctl_lock);
		if (!omnia->executor || pid_vnr(omnia->executor) != current->tgid)
			ret = -EPERM;
		else
			omnia->last_heartbeat_ns = omnia_now_ns();
		mutex_unlock(&omnia->ctl_lock);
		return ret;
	}

	default:
		return -ENOTTY;
	}
}

static const struct file_operations omnia_fops = {
	.owner		= THIS_MODULE,
	.open		= omnia_open,
	.release	= omnia_release,
	.read		= omnia_read,
	.poll		= omnia_poll,
	.unlocked_ioctl	= omnia_ioctl,
	.compat_ioctl	= compat_ptr_ioctl,
	.llseek		= no_llseek,
};

static struct miscdevice omnia_misc = {
	.minor	= MISC_DYNAMIC_MINOR,
	.name	= OMNIA_DEVICE_NAME,
	.fops	= &omnia_fops,
	.mode	= 0600,		/* root only; the autonomous daemon is root */
};

/* ------------------------------------------------------------------------ */

static int __init omnia_init(void)
{
	int ret;

	if (!is_power_of_2(ring_events) || ring_events < 64) {
		pr_err("omnia: ring_events must be a power of two >= 64\n");
		return -EINVAL;
	}

	omnia = kzalloc(sizeof(*omnia), GFP_KERNEL);
	if (!omnia)
		return -ENOMEM;

	ret = kfifo_alloc(&omnia->fifo, ring_events * sizeof(struct omnia_event),
			  GFP_KERNEL);
	if (ret)
		goto err_free;

	spin_lock_init(&omnia->fifo_lock);
	mutex_init(&omnia->ctl_lock);
	init_waitqueue_head(&omnia->readers);
	atomic64_set(&omnia->seq, 0);
	omnia->guard.state = OMNIA_GUARD_ABSENT;

	timer_setup(&omnia->watchdog, omnia_watchdog_fn, 0);
	mod_timer(&omnia->watchdog,
		  jiffies + msecs_to_jiffies(OMNIA_WATCHDOG_SECS * 1000));

	ret = misc_register(&omnia_misc);
	if (ret)
		goto err_timer;

	pr_info("omnia: /dev/%s ready, abi %u, ring %u events (%zu KiB)\n",
		OMNIA_DEVICE_NAME, OMNIA_ABI_VERSION, ring_events,
		(ring_events * sizeof(struct omnia_event)) >> 10);
	return 0;

err_timer:
	del_timer_sync(&omnia->watchdog);
	kfifo_free(&omnia->fifo);
err_free:
	kfree(omnia);
	omnia = NULL;
	return ret;
}

static void __exit omnia_exit(void)
{
	misc_deregister(&omnia_misc);
	del_timer_sync(&omnia->watchdog);
	if (omnia->executor)
		put_pid(omnia->executor);
	kfifo_free(&omnia->fifo);
	kfree(omnia);
	omnia = NULL;
	pr_info("omnia: unloaded\n");
}

module_init(omnia_init);
module_exit(omnia_exit);
