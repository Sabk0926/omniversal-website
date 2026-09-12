# Omnia OS

An Ubuntu 24.04 derivative built on one idea:

> You never need to know a command. If the OS doesn't have a capability, it builds one,
> tests it, and keeps it.

Ask for nightly photo backups on a machine with no backup tool. You don't get advice or a
shell command. You get a backup tool: built, tested to prove a file actually restores,
packaged, installed. The machine has backup now. So does any machine you copy that package
to.

Plug in a device with no driver and it works down a ladder, from `modprobe` up to writing
a sandboxed userspace driver, keeping it only if the device returns sane data.

This isn't Ubuntu with a chatbot in the dock.

## Why generated things become .deb packages

Anything the OS builds has to be undoable, explainable and copyable. Packages already do
that:

| Need | Answer |
|---|---|
| Undo it | `apt remove` |
| Why is this here? | `dpkg -l` plus a provenance file |
| Put it on my laptop too | copy the file |
| Did an upgrade break it? | its test is kept and re-run |

A custom registry would mean rewriting packaging, worse.

## How it builds something

| Step | What happens |
|---|---|
| 1 | Do we already have this? If yes, use it. Stops the junk drawer of twelve backup scripts |
| 2 | Build it from vetted parts if possible. The small local model wires them. Genuinely new code escalates to a bigger model |
| 3 | Write the permissions first: which paths, devices, syscalls, network. Anything unlisted is blocked |
| 4 | The model writes a test proving it works. A backup must restore a file byte-for-byte. Test fails, nothing installs |
| 5 | Package it, install it, keep the test |

Step 4 is what makes "the OS built itself a backup" reassuring rather than alarming.

## It fixes itself

One rule: **nothing audits itself.** Capabilities are checked by the daemon, the daemon by
the kernel, the kernel from off-box.

**Diagnosing.** Because every capability keeps the test that proved it, the machine builds
up a working definition of "healthy" that grows as it learns things. `omni doctor` runs
everything the machine claims it can do. Normal monitoring tells you the backup process
exited 0; it can't tell you the backup can be restored.

**Healing.** A kernel upgrade breaks a generated USB driver. Its test fails on next boot.
The OS rebuilds that driver against the new kernel and re-runs the test. Passes, nobody
gets paged. Fails, it rolls the kernel back and then tells you.

**Learning.** In order of how much you can inspect: capabilities first, then per-host
baselines and an incident history, and only last a LoRA adapter trained on a builder box
from verified outcomes. Weights are the only level you can't selectively delete, so
they're last, not the headline.

## Architecture

The kernel notices. Userspace reasons. No inference in ring 0.

```
   kernel                                     userspace
 ┌────────────────────────────┐
 │ eBPF probes                │──events──►┌─────────────────────────────┐
 │  exec · oom · block · sig  │           │ omniad                       │
 ├────────────────────────────┤           │  forge · registry · sandbox  │
 │ omnia_kmod → /dev/omnia    │◄──ioctl───┤  autonomy · undo · audit     │
 │  event ring, executor claim│           └──────────────┬──────────────┘
 ├────────────────────────────┤                          │
 │ BPF-LSM guard              │                   ┌──────▼───────┐
 │  the things it must never  │                   │ omnia-modeld │
 │  do, enforced below it     │                   │ llama.cpp    │
 └────────────────────────────┘                   └──────────────┘
```

The guard is in the kernel because the daemon runs as root and acts unattended. A rule the
daemon enforces on itself is one bad generation away from not existing. The guard is
pinned before `omniad` starts, in a cgroup it can't edit, and `omniad` refuses to act
autonomously unless the guard reports active.

## Status

Alpha, in design. See [docs/DESIGN.md](docs/DESIGN.md).

| Part | State |
|---|---|
| `kernel/omnia-kmod/` — char device, event ring, executor claim, watchdog | written, ~470 lines C |
| `kernel/bpf/` — probes + BPF-LSM floor | written, ~470 lines C |
| `runtime/` — Rust workspace, 15 crates | stubs, builds clean |
| `images/` — amd64 ISO + arm64 flashable | not started |

Nothing boots yet. The kernel layer is the part that's real.

## Targets

The floor is an 8 GB ARM64 board running a 3-4B model at int4, about 2-2.5 GB resident.
If it doesn't work there, it isn't in the design. There's enough headroom left to load a
7B transiently, so the floor target can handle its own escalation rather than always
needing a bigger machine.

Clears the floor: Pi 5 8GB, Orange Pi 5, Radxa Rock 5B, Jetson Orin Nano 8GB. Rules out
Pi 4, Pi Zero and most cheap industrial boards.

The same source produces an amd64 ISO for desktops and a flashable arm64 image for
boards.

## Licence

GPL-3.0-or-later, same as the Ubuntu base.
