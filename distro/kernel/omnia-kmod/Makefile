# Out-of-tree build. `make` against the running kernel, or KDIR=... for a
# cross/DKMS build.
obj-m += omnia_kmod.o

KDIR ?= /lib/modules/$(shell uname -r)/build
PWD  := $(shell pwd)

all:
	$(MAKE) -C $(KDIR) M=$(PWD) modules

clean:
	$(MAKE) -C $(KDIR) M=$(PWD) clean

install: all
	$(MAKE) -C $(KDIR) M=$(PWD) modules_install

.PHONY: all clean install
