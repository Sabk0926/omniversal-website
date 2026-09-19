# Building the kernel layer

Both halves compile. This is the exact recipe, verified end to end on Ubuntu
24.04, including inside a container whose *running* kernel is not the target.

## What you need

```bash
apt-get install -y linux-headers-generic linux-tools-generic libbpf-dev clang
```

That is all. No hardware, no matching kernel, no special privileges beyond
installing packages.

## The module

```bash
cd omnia-kmod
make -C /lib/modules/$(uname -r)/build M=$PWD modules
```

Produces `omnia_kmod.ko`. Builds warning-clean.

**You do not need to be running the kernel you build for.** The module targets
Ubuntu 24.04's 6.8 kernel, and `linux-headers-generic` installs those headers on
any machine. Building against them proves the C is correct against the real
kernel API; *loading* it is the separate step that needs a matching machine.

To build against a specific kernel rather than the running one:

```bash
make -C /lib/modules/6.8.0-139-generic/build M=$PWD modules
```

In production this is DKMS's job — see `dkms.conf`. DKMS rebuilds the module on
the user's machine against whatever kernel they are actually running, which is
why the ABI parity test in `runtime/crates/omnia-abi` exists: that rebuild and
the userspace package are compiled months apart.

## The BPF objects

```bash
cd bpf
make
```

`vmlinux.h` is generated first, from the build host's BTF:

```bash
bpftool btf dump file /sys/kernel/btf/vmlinux format c > vmlinux.h
```

The resulting objects are still portable. CO-RE relocations resolve against the
*running* kernel's BTF at load time, so an object built here loads on a Pi.

### If bpftool says it cannot find itself

`/usr/sbin/bpftool` is a wrapper that dispatches to a build matching the
*running* kernel. On a machine running something other than an Ubuntu kernel —
a container on a custom hypervisor kernel, say — that lookup fails. Call the
real binary:

```bash
/usr/lib/linux-tools/6.8.0-139-generic/bpftool btf dump file \
    /sys/kernel/btf/vmlinux format c > vmlinux.h
```

`/sys/kernel/btf/vmlinux` is the *running* kernel's BTF and is what you want
regardless: it describes the kernel whose structures the programs will read.

## What compiling does and does not prove

| Proven | Not proven |
|---|---|
| The C is valid against a real kernel API | The module loads |
| Every struct and tracepoint the BPF reads exists in BTF | The hooks attach |
| The LSM hooks and tracepoints are well-formed sections | The guard denies what it should |
| `omnia_abi.h` agrees with the Rust side | Any of it survives a reboot |

The right-hand column needs a machine you can load a module on. Everything in
the left-hand column was reached with nothing but `apt-get install`.

Two bugs were caught the first time this was actually compiled, both invisible
to inspection:

- `kfifo_out` is `__must_check`, and the eviction path discarded its result. A
  short dequeue would have left the ring in a state the following `kfifo_in`
  papered over, attributing the failure to the new event rather than to the
  eviction.
- `omnia_probes.bpf.c` used `struct trace_event_raw_block_rq_error`, which does
  not exist. `block_rq_error` is a `DEFINE_EVENT` on the `block_rq_completion`
  class, so the kernel generates the struct under the *class* name. The name
  that looks right compiles only until it meets a real `vmlinux.h`.
