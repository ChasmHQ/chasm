use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub struct OwnedNode {
    active: AtomicBool,
    handle: usize,
    port: u16,
}

impl OwnedNode {
    pub fn from_child_handle(handle: usize, port: u16) -> Arc<Self> {
        Arc::new(Self {
            active: AtomicBool::new(true),
            handle,
            port,
        })
    }

    pub fn valid(&self) -> bool {
        if !self.active.load(Ordering::Acquire) {
            return false;
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::{
                Foundation::WAIT_TIMEOUT,
                System::Threading::{GetProcessId, WaitForSingleObject},
            };
            let handle = self.handle as _;
            let live = unsafe { WaitForSingleObject(handle, 0) == WAIT_TIMEOUT };
            let pid = unsafe { GetProcessId(handle) };
            live && pid != 0 && owns_listener(pid, self.port)
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    pub fn authorizes_loopback(&self, port: u16) -> bool {
        self.port == port && self.valid()
    }

    pub fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

#[cfg(windows)]
fn owns_listener(pid: u32, port: u16) -> bool {
    use windows_sys::Win32::{
        Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR},
        NetworkManagement::IpHelper::{
            GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
        },
        Networking::WinSock::AF_INET,
    };
    let mut length = 0;
    let status = unsafe {
        GetExtendedTcpTable(
            std::ptr::null_mut(),
            &mut length,
            0,
            AF_INET as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };
    if status != ERROR_INSUFFICIENT_BUFFER || !(4..=16 * 1024 * 1024).contains(&length) {
        return false;
    }
    let mut table = vec![0u8; length as usize];
    let status = unsafe {
        GetExtendedTcpTable(
            table.as_mut_ptr().cast(),
            &mut length,
            0,
            AF_INET as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };
    if status != NO_ERROR || length < 4 || length as usize > table.len() {
        return false;
    }
    let count = u32::from_ne_bytes(table[..4].try_into().unwrap()) as usize;
    let row_size = std::mem::size_of::<MIB_TCPROW_OWNER_PID>();
    let Some(bytes) = count
        .checked_mul(row_size)
        .and_then(|bytes| bytes.checked_add(4))
    else {
        return false;
    };
    if bytes > length as usize {
        return false;
    }
    (0..count).any(|index| {
        // The byte buffer is not guaranteed to have the alignment of the Windows row type.
        let row = unsafe {
            std::ptr::read_unaligned(
                table
                    .as_ptr()
                    .add(4 + index * row_size)
                    .cast::<MIB_TCPROW_OWNER_PID>(),
            )
        };
        row.dwOwningPid == pid
            && u16::from_be(row.dwLocalPort as u16) == port
            && [std::net::Ipv4Addr::LOCALHOST.octets(), [0, 0, 0, 0]]
                .contains(&row.dwLocalAddr.to_ne_bytes())
    })
}

impl Drop for OwnedNode {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle as _);
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use windows_sys::Win32::{
        Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS},
        System::Threading::GetCurrentProcess,
    };

    fn current_process_node(port: u16) -> Arc<OwnedNode> {
        let process = unsafe { GetCurrentProcess() };
        let mut handle = std::ptr::null_mut();
        assert_ne!(
            unsafe {
                DuplicateHandle(
                    process,
                    process,
                    process,
                    &mut handle,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            },
            0
        );
        OwnedNode::from_child_handle(handle as usize, port)
    }

    #[test]
    fn capability_requires_live_owned_listener_and_can_be_revoked() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let node = current_process_node(listener.local_addr().unwrap().port());
        assert!(node.valid());
        node.revoke();
        assert!(!node.valid());
    }

    #[test]
    fn liveness_or_same_chain_alone_does_not_authorize_an_unowned_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!owns_listener(u32::MAX, port));
        let node = current_process_node(port);
        drop(listener);
        assert!(!node.valid());
        let invalid = OwnedNode::from_child_handle(0, port);
        assert!(!invalid.valid());
    }
}
