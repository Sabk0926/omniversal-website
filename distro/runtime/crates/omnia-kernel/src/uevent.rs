//! Uevents: the kernel saying a device just appeared.
//!
//! # Why this and not enumeration
//!
//! Walking sysfs answers "what is on this machine". That is the wrong question
//! for the driver ladder, and running it against a real machine shows why: a
//! healthy cloud instance has ten devices with no driver bound — serial ports,
//! a PC speaker, an RTC — and none of them is a problem to solve. They have
//! been that way since boot and the machine works.
//!
//! The ladder's actual trigger is a *change*: you plugged something in, and it
//! did not work. That is a uevent, and it carries its own justification for
//! acting. Enumeration stays useful as a survey — `omni doctor` asking what is
//! unsupported here — but it is not what wakes the forge.
//!
//! # Trusting the sender
//!
//! Netlink is a socket, and a message arriving on it is not automatically from
//! the kernel. A local process can send one. If it were believed, an
//! unprivileged user could describe a device that does not exist and steer
//! whatever the machine does next — which here means driver generation.
//!
//! udev learned this the hard way and the fix is two checks, both of which
//! [`UeventSocket`] makes: the netlink source port must be 0, which is the
//! kernel, and the SCM_CREDENTIALS uid attached by the kernel to every message
//! must be 0. A message failing either is dropped without being parsed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A device appeared. The ladder's trigger.
    Add,
    Remove,
    Change,
    Move,
    /// A driver attached to a device. This is what success looks like, and
    /// what cancels a ladder run that is already under way.
    Bind,
    /// A driver detached. Not the same as the device leaving.
    Unbind,
    Online,
    Offline,
    Unknown,
}

impl Action {
    fn parse(raw: &str) -> Action {
        match raw {
            "add" => Action::Add,
            "remove" => Action::Remove,
            "change" => Action::Change,
            "move" => Action::Move,
            "bind" => Action::Bind,
            "unbind" => Action::Unbind,
            "online" => Action::Online,
            "offline" => Action::Offline,
            _ => Action::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Add => "add",
            Action::Remove => "remove",
            Action::Change => "change",
            Action::Move => "move",
            Action::Bind => "bind",
            Action::Unbind => "unbind",
            Action::Online => "online",
            Action::Offline => "offline",
            Action::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uevent {
    pub action: Action,
    /// Kernel path, e.g. `/devices/pci0000:00/0000:00:04.0/usb1/1-1`.
    pub devpath: String,
    pub subsystem: Option<String>,
    pub properties: BTreeMap<String, String>,
}

impl Uevent {
    /// Parse one netlink message.
    ///
    /// The kernel format is `ACTION@DEVPATH\0KEY=VALUE\0KEY=VALUE\0...`. The
    /// header is redundant with the `ACTION` and `DEVPATH` properties that
    /// follow, and they are what is used: a message whose header and properties
    /// disagree is malformed, and preferring the header would mean parsing a
    /// device path that the rest of the message contradicts.
    pub fn parse(message: &[u8]) -> Option<Uevent> {
        // libudev's own monitor format, which is what a message relayed by
        // udev rather than the kernel looks like. Different framing, and not
        // something to half-parse.
        if message.starts_with(b"libudev\0") {
            return None;
        }

        let mut properties = BTreeMap::new();
        let mut fields = message.split(|byte| *byte == 0);

        // The header, kept only to fall back on.
        let header = fields.next()?;
        let header = String::from_utf8_lossy(header);
        let (header_action, header_devpath) = header.split_once('@')?;

        for field in fields {
            if field.is_empty() {
                continue;
            }
            let text = String::from_utf8_lossy(field);
            if let Some((key, value)) = text.split_once('=') {
                properties.insert(key.to_string(), value.to_string());
            }
        }

        let action = properties
            .get("ACTION")
            .map_or_else(|| Action::parse(header_action), |raw| Action::parse(raw));
        let devpath = properties
            .get("DEVPATH")
            .cloned()
            .unwrap_or_else(|| header_devpath.to_string());

        if devpath.is_empty() {
            return None;
        }

        Some(Uevent {
            action,
            devpath,
            subsystem: properties.get("SUBSYSTEM").cloned(),
            properties,
        })
    }

    /// Where this device lives under a sysfs root.
    ///
    /// `DEVPATH` is absolute in kernel terms but relative to the sysfs mount,
    /// so it is joined after stripping the leading slash. Joining it directly
    /// would discard the root and silently read the real `/sys` during a test.
    pub fn syspath(&self, sys_root: &Path) -> PathBuf {
        sys_root.join(self.devpath.trim_start_matches('/'))
    }

    pub fn modalias(&self) -> Option<crate::Modalias> {
        self.properties
            .get("MODALIAS")
            .map(|raw| crate::Modalias::parse(raw))
    }

    /// Is this the event that should start a ladder run?
    ///
    /// Only `add`, and only for a device the kernel published a modalias for.
    /// An `add` with no modalias is a bus, a partition or a virtual device —
    /// things that appear constantly on a running machine and never need a
    /// driver written.
    ///
    /// Note that a driver binding normally arrives as a *separate* `bind`
    /// event a moment later, so seeing this return true does not mean the
    /// device is unsupported. It means the question is now open, and the
    /// caller should give the kernel its moment before climbing.
    pub fn opens_the_question(&self) -> bool {
        self.action == Action::Add && self.properties.contains_key("MODALIAS")
    }

    pub fn describe(&self) -> String {
        let what = self
            .modalias()
            .map(|alias| alias.describe())
            .or_else(|| self.subsystem.clone())
            .unwrap_or_else(|| "device".into());
        format!("{} {} at {}", self.action, what, self.devpath)
    }
}

#[cfg(target_os = "linux")]
pub use socket::UeventSocket;

#[cfg(target_os = "linux")]
mod socket {
    //! The netlink socket. The only unsafe in this crate, kept in one place.

    use std::io;
    use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::time::Duration;

    use super::Uevent;

    /// `NETLINK_KOBJECT_UEVENT`. Not in libc's constants on every version.
    const NETLINK_KOBJECT_UEVENT: libc::c_int = 15;
    /// Multicast group 1 is the kernel's. Binding to it needs CAP_NET_ADMIN.
    const KERNEL_GROUP: u32 = 1;
    /// A burst of uevents on a USB hub plug-in is larger than the default.
    /// Dropping one is dropping a device.
    const RECEIVE_BUFFER: libc::c_int = 2 * 1024 * 1024;

    pub struct UeventSocket {
        fd: OwnedFd,
    }

    impl UeventSocket {
        /// Open and bind. Needs CAP_NET_ADMIN, which is why the daemon opens
        /// this once at startup rather than on demand.
        pub fn open() -> io::Result<UeventSocket> {
            // SAFETY: socket(2) with constant arguments; the returned fd is
            // checked and immediately given to OwnedFd, which closes it.
            let raw: RawFd = unsafe {
                libc::socket(
                    libc::AF_NETLINK,
                    libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                    NETLINK_KOBJECT_UEVENT,
                )
            };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `raw` is a fresh, valid, owned descriptor.
            let fd = unsafe { OwnedFd::from_raw_fd(raw) };

            set_option(&fd, libc::SO_RCVBUFFORCE, RECEIVE_BUFFER)
                .or_else(|_| set_option(&fd, libc::SO_RCVBUF, RECEIVE_BUFFER))?;
            // The kernel attaches the sender's credentials to every message
            // only if this is set. Without it there is nothing to check.
            set_option(&fd, libc::SO_PASSCRED, 1)?;

            let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
            address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
            address.nl_groups = KERNEL_GROUP;

            // SAFETY: `address` is a correctly initialised sockaddr_nl and the
            // length passed is its own size.
            let bound = unsafe {
                libc::bind(
                    fd.as_raw_fd(),
                    std::ptr::addr_of!(address).cast::<libc::sockaddr>(),
                    std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
                )
            };
            if bound < 0 {
                return Err(io::Error::last_os_error());
            }

            Ok(UeventSocket { fd })
        }

        /// Block until a message arrives, or the timeout elapses.
        ///
        /// `Ok(None)` means the timeout elapsed or a message was dropped for
        /// failing its credential check. Those are deliberately the same to the
        /// caller: neither is an error, and neither yields an event.
        pub fn next_event(&self, timeout: Duration) -> io::Result<Option<Uevent>> {
            let mut poll = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let milliseconds = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;

            // SAFETY: one pollfd, count matches, and the struct outlives the
            // call.
            let ready = unsafe { libc::poll(&mut poll, 1, milliseconds) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                // A signal during poll is not a failure; the caller loops.
                if error.kind() == io::ErrorKind::Interrupted {
                    return Ok(None);
                }
                return Err(error);
            }
            if ready == 0 {
                return Ok(None);
            }
            self.receive()
        }

        /// One `recvmsg`, with the credential checks that make the message
        /// worth believing.
        fn receive(&self) -> io::Result<Option<Uevent>> {
            let mut buffer = vec![0u8; 8192];
            let mut source: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
            let mut control = [0u8; 64];

            let mut iov = libc::iovec {
                iov_base: buffer.as_mut_ptr().cast::<libc::c_void>(),
                iov_len: buffer.len(),
            };
            let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
            message.msg_name = std::ptr::addr_of_mut!(source).cast::<libc::c_void>();
            message.msg_namelen = std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t;
            message.msg_iov = &mut iov;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast::<libc::c_void>();
            message.msg_controllen = control.len() as _;

            // SAFETY: every pointer in `message` refers to a live local that
            // outlives the call, and each length matches its buffer.
            let received = unsafe { libc::recvmsg(self.fd.as_raw_fd(), &mut message, 0) };
            if received < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    return Ok(None);
                }
                return Err(error);
            }

            // Check one: the netlink source port. 0 is the kernel; anything
            // else is a process on this machine.
            if source.nl_pid != 0 {
                return Ok(None);
            }

            // Check two: the credentials the kernel attached. A local process
            // can set its own nl_pid, so the port number alone is not enough.
            if !sender_is_root(&message) {
                return Ok(None);
            }

            Ok(Uevent::parse(&buffer[..received as usize]))
        }
    }

    /// Walk the control messages for SCM_CREDENTIALS and check the uid.
    ///
    /// Absent credentials fail closed. The kernel attaches them to every
    /// message once SO_PASSCRED is set, so "no credentials" means either the
    /// option did not take or something is forging framing — neither is a
    /// reason to trust the payload.
    fn sender_is_root(message: &libc::msghdr) -> bool {
        // SAFETY: `message` came from a successful recvmsg, so its control
        // buffer and length describe initialised memory.
        let mut header = unsafe { libc::CMSG_FIRSTHDR(message) };
        while !header.is_null() {
            // SAFETY: CMSG_FIRSTHDR/CMSG_NXTHDR only return pointers into the
            // control buffer that are valid to read.
            let current = unsafe { &*header };
            if current.cmsg_level == libc::SOL_SOCKET && current.cmsg_type == libc::SCM_CREDENTIALS
            {
                // SAFETY: a SCM_CREDENTIALS message's data is a ucred.
                let credentials = unsafe {
                    std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<libc::ucred>())
                };
                return credentials.uid == 0;
            }
            // SAFETY: same invariants as above.
            header = unsafe { libc::CMSG_NXTHDR(message, header) };
        }
        false
    }

    fn set_option(fd: &OwnedFd, option: libc::c_int, value: libc::c_int) -> io::Result<()> {
        // SAFETY: `value` is a live c_int and the length passed is its size.
        let result = unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                std::ptr::addr_of!(value).cast::<libc::c_void>(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a kernel-format message: header, then NUL-separated properties.
    fn message(header: &str, properties: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(header.as_bytes());
        out.push(0);
        for property in properties {
            out.extend_from_slice(property.as_bytes());
            out.push(0);
        }
        out
    }

    #[test]
    fn a_usb_plug_in_parses_into_something_the_ladder_can_act_on() {
        let raw = message(
            "add@/devices/pci0000:00/0000:00:04.0/usb1/1-1",
            &[
                "ACTION=add",
                "DEVPATH=/devices/pci0000:00/0000:00:04.0/usb1/1-1",
                "SUBSYSTEM=usb",
                "MODALIAS=usb:v0BDAp8153d0100dc00dsc00dp00ic02isc06ip00in00",
                "PRODUCT=bda/8153/100",
                "SEQNUM=4242",
            ],
        );

        let event = Uevent::parse(&raw).unwrap();
        assert_eq!(event.action, Action::Add);
        assert_eq!(event.subsystem.as_deref(), Some("usb"));
        assert!(event.opens_the_question());
        assert_eq!(event.modalias().unwrap().id_pair().unwrap(), "0bda:8153");
        assert!(
            event.describe().contains("0bda:8153"),
            "{}",
            event.describe()
        );
    }

    #[test]
    fn the_properties_win_over_the_header_when_they_disagree() {
        // They are redundant, and a message where they differ is malformed.
        // Preferring the header would mean acting on a path the rest of the
        // message contradicts.
        let raw = message(
            "add@/devices/spoofed",
            &["ACTION=remove", "DEVPATH=/devices/real", "SUBSYSTEM=usb"],
        );
        let event = Uevent::parse(&raw).unwrap();
        assert_eq!(event.action, Action::Remove);
        assert_eq!(event.devpath, "/devices/real");
    }

    #[test]
    fn a_message_with_only_a_header_still_parses() {
        // Older kernels and some synthetic events are this sparse.
        let event = Uevent::parse(&message("add@/devices/platform/x", &[])).unwrap();
        assert_eq!(event.action, Action::Add);
        assert_eq!(event.devpath, "/devices/platform/x");
        assert!(
            !event.opens_the_question(),
            "no modalias means nothing to match on"
        );
    }

    #[test]
    fn a_bind_event_is_not_a_reason_to_climb_the_ladder() {
        // It is the opposite: a driver just attached.
        let raw = message(
            "bind@/devices/x",
            &["ACTION=bind", "DEVPATH=/devices/x", "MODALIAS=usb:v1p2"],
        );
        let event = Uevent::parse(&raw).unwrap();
        assert_eq!(event.action, Action::Bind);
        assert!(!event.opens_the_question());
    }

    #[test]
    fn an_add_with_no_modalias_is_ignored() {
        // Partitions, virtual devices and bus objects appear constantly and
        // none of them wants a driver written.
        let raw = message(
            "add@/devices/virtual/block/loop0",
            &[
                "ACTION=add",
                "DEVPATH=/devices/virtual/block/loop0",
                "SUBSYSTEM=block",
            ],
        );
        assert!(!Uevent::parse(&raw).unwrap().opens_the_question());
    }

    #[test]
    fn a_libudev_framed_message_is_refused_rather_than_half_parsed() {
        let mut raw = b"libudev\0".to_vec();
        raw.extend_from_slice(&[0xfe, 0xed, 0xca, 0xfe]);
        raw.extend_from_slice(b"ACTION=add\0DEVPATH=/devices/x\0");
        assert!(Uevent::parse(&raw).is_none());
    }

    #[test]
    fn a_message_with_no_devpath_is_refused() {
        // Nothing to look at in sysfs means nothing to act on.
        assert!(Uevent::parse(&message("add@", &["ACTION=add"])).is_none());
        assert!(Uevent::parse(b"not a uevent at all").is_none());
        assert!(Uevent::parse(b"").is_none());
    }

    #[test]
    fn an_unknown_action_is_named_rather_than_guessed_at() {
        let raw = message("teleport@/devices/x", &["DEVPATH=/devices/x"]);
        let event = Uevent::parse(&raw).unwrap();
        assert_eq!(event.action, Action::Unknown);
        assert!(!event.opens_the_question());
    }

    #[test]
    fn devpath_is_joined_under_the_given_root_not_the_real_sysfs() {
        // DEVPATH starts with a slash. Joining it directly discards the root,
        // which in a test means silently reading the machine's own /sys.
        let event = Uevent::parse(&message(
            "add@/devices/platform/x",
            &["DEVPATH=/devices/platform/x"],
        ))
        .unwrap();
        assert_eq!(
            event.syspath(Path::new("/tmp/fake-sys")),
            Path::new("/tmp/fake-sys/devices/platform/x")
        );
    }

    #[test]
    fn a_property_value_containing_an_equals_sign_survives() {
        let raw = message(
            "add@/devices/x",
            &["DEVPATH=/devices/x", "OF_COMPATIBLE_0=vendor,part=rev1"],
        );
        let event = Uevent::parse(&raw).unwrap();
        assert_eq!(event.properties["OF_COMPATIBLE_0"], "vendor,part=rev1");
    }

    #[test]
    fn trailing_nuls_do_not_produce_empty_properties() {
        let mut raw = message("add@/devices/x", &["DEVPATH=/devices/x"]);
        raw.extend_from_slice(&[0, 0, 0]);
        let event = Uevent::parse(&raw).unwrap();
        assert_eq!(event.properties.len(), 1);
    }
}
