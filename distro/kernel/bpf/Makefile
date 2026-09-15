# BPF CO-RE build. Produces two objects plus the vmlinux.h they compile against.
#
# CO-RE means these objects are built once and load on any kernel new enough to
# have the hooks -- which matters here because the same .deb must work on an
# Ubuntu desktop kernel, a Raspberry Pi kernel and a vendor BSP kernel.

CLANG   ?= clang
BPFTOOL ?= bpftool
ARCH    := $(shell uname -m | sed 's/x86_64/x86/;s/aarch64/arm64/;s/armv7l/arm/')

OBJS := omnia_probes.bpf.o omnia_guard.bpf.o

CFLAGS := -g -O2 -target bpf -D__TARGET_ARCH_$(ARCH) \
          -Wall -Werror -Wno-unused-function -I.

all: vmlinux.h $(OBJS)

# Generated from the BUILD host's kernel BTF. The resulting objects are still
# portable: CO-RE relocations are resolved against the RUNNING kernel's BTF at
# load time, not against this file.
vmlinux.h:
	$(BPFTOOL) btf dump file /sys/kernel/btf/vmlinux format c > $@

%.bpf.o: %.bpf.c omnia_bpf.h vmlinux.h
	$(CLANG) $(CFLAGS) -c $< -o $@
	@$(BPFTOOL) gen object $@ $@ 2>/dev/null || true

verify: $(OBJS)
	@for obj in $(OBJS); do \
		echo "== $$obj"; \
		$(BPFTOOL) prog load $$obj /sys/fs/bpf/omnia-verify-test 2>&1 | head -20; \
		rm -f /sys/fs/bpf/omnia-verify-test; \
	done

clean:
	rm -f $(OBJS) vmlinux.h

.PHONY: all verify clean
